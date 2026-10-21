//! The code view in a GPUI test window: what a frame shapes (the deterministic half of B13),
//! restyling without a reparse, find in file, mouse selection and copy.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AppContext as _, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px,
    size,
};
use hutan_code_view::{CodeStyle, CodeView, FileBuffer, FindQuery, OVERSCAN_LINES};
use hutan_settings::themes::{Builtin, builtin_theme};

/// Lines of the big file.
const LINES: usize = 50_000;

fn style(builtin: Builtin) -> CodeStyle {
    CodeStyle::new(builtin_theme(builtin).editor(), "Menlo", 13.0, 1.4)
}

fn rust_file(lines: usize) -> String {
    let mut text = String::new();
    for n in 0..lines {
        match n % 5 {
            0 => text.push_str(&format!("/// Item {n}\n")),
            1 => text.push_str(&format!("pub fn item_{n}(x: u32) -> u32 {{\n")),
            2 => text.push_str(&format!("    let s = \"value {n}\";\n")),
            3 => text.push_str("    x + 1\n"),
            _ => text.push_str("}\n"),
        }
    }
    text
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
}

/// A window of 900 × 600 with a view of a `name` file holding `text`.
fn open<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    text: &str,
    syntax: bool,
) -> (
    Entity<CodeView>,
    Entity<FileBuffer>,
    &'a mut VisualTestContext,
) {
    // The parser's thread wakes the view from outside the test scheduler.
    cx.executor().allow_parking();
    let text = text.to_owned();
    let path = PathBuf::from(format!("/virtual/{name}"));
    let buffer = cx.new(|cx| FileBuffer::new(path, cx));
    let shared = buffer.clone();
    let (view, cx) =
        cx.add_window_view(move |_, cx| CodeView::new(shared, style(Builtin::HutanDark), cx));
    cx.simulate_resize(size(px(900.0), px(600.0)));
    draw(cx);
    // The text arrives once the view is on screen, as it does from the daemon.
    buffer.update(cx, |buffer, cx| {
        buffer.set_loaded(&text, false, cx);
        buffer.set_syntax_active(syntax, cx);
    });
    draw(cx);
    (view, buffer, cx)
}

fn rows(view: &Entity<CodeView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout().expect("laid out").rows)
}

/// Lines the view has shaped so far (a frame may be prepared more than once; what counts is
/// what a draw costs).
fn shaped(view: &Entity<CodeView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.stats().lines_shaped as usize)
}

#[gpui::test]
fn a_frame_shapes_only_the_lines_in_view(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "big.rs", &rust_file(LINES), false);
    let rows = rows(&view, cx);
    let budget = rows + 2 * OVERSCAN_LINES;
    let drawn = view.read_with(cx, |v, _| v.stats().last_lines.clone());
    let first = shaped(&view, cx);
    assert!(
        drawn.len() <= budget && first <= budget,
        "opening a {LINES}-line file drew {drawn:?} and shaped {first} lines; the view holds {rows}",
    );

    // Nothing changed: nothing is shaped again.
    draw(cx);
    assert_eq!(shaped(&view, cx), first);

    // The middle of the file: one view's worth of lines, starting there.
    view.update(cx, |v, cx| v.scroll_to(25_000.0 * 18.0, cx));
    draw(cx);
    let drawn = view.read_with(cx, |v, _| v.stats().last_lines.clone());
    let middle = shaped(&view, cx) - first;
    assert!(drawn.start > 20_000, "{drawn:?}");
    assert!(middle <= budget, "the middle shaped {middle} lines");

    // One line further: only the line that came into view is new.
    let before = shaped(&view, cx);
    view.update(cx, |v, cx| v.scroll_lines(1.0, cx));
    draw(cx);
    let step = shaped(&view, cx) - before;
    assert!(step <= 1, "scrolling by a line shaped {step} lines");
}

#[gpui::test]
fn a_theme_change_restyles_without_a_reparse(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "lib.rs", &rust_file(2_000), true);
    assert!(
        buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))),
        "the tree of the file is published"
    );
    draw(cx);
    draw(cx);
    assert!(
        view.read_with(cx, |v, _| v.paint_times().first_highlighted)
            .is_some(),
        "a frame drew highlighted lines"
    );
    let parses = buffer.read_with(cx, |b, _| b.syntax_stats().expect("syntax").started);
    let rows = rows(&view, cx);
    let before = shaped(&view, cx);

    view.update(cx, |v, cx| v.set_style(style(Builtin::HutanLight), cx));
    draw(cx);
    let drawn = view.read_with(cx, |v, _| v.stats().last_lines.len());
    let restyled = shaped(&view, cx) - before;
    assert!(
        restyled >= drawn && restyled <= rows,
        "every line in view ({drawn}) is drawn again in the new colors, no more: {restyled}"
    );
    assert_eq!(
        buffer.read_with(cx, |b, _| b.syntax_stats().expect("syntax").started),
        parses,
        "a new theme parses nothing"
    );
}

#[gpui::test]
fn syntax_is_kept_only_while_shown(cx: &mut TestAppContext) {
    let (_view, buffer, cx) = open(cx, "lib.rs", &rust_file(500), true);
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    let tree = std::sync::Arc::downgrade(
        &buffer
            .read_with(cx, |b, _| b.syntax_snapshot())
            .expect("a tree"),
    );
    buffer.update(cx, |b, cx| b.set_syntax_active(false, cx));
    assert!(!buffer.read_with(cx, |b, _| b.has_syntax()));
    // The parser's thread lets go of the tree when it notices it was dropped.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while tree.upgrade().is_some() {
        assert!(std::time::Instant::now() < deadline, "the tree is freed");
        std::thread::yield_now();
    }
    // Shown again: parsed again.
    buffer.update(cx, |b, cx| b.set_syntax_active(true, cx));
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));

    // Large-file mode never parses.
    buffer.update(cx, |b, cx| {
        let text = b.text().to_string();
        b.set_loaded(&text, true, cx);
    });
    assert!(!buffer.read_with(cx, |b, _| b.has_syntax()));
}

#[gpui::test]
fn find_steps_through_the_matches(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "big.rs", &rust_file(LINES), false);
    view.update(cx, |v, cx| {
        v.set_find_query(FindQuery::literal("ITEM_4999"), cx)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while view.read_with(cx, |v, _| v.is_searching()) {
        assert!(std::time::Instant::now() < deadline);
        cx.run_until_parked();
    }
    let (summary, top) = view.read_with(cx, |v, _| (v.find_state().summary(), v.scroll_y()));
    // item_49991 and item_49996 (only lines ≡ 1 mod 5 name an item), found case-insensitively.
    assert_eq!(summary, "1 of 2");
    assert!(top > 0.0, "the first match was scrolled into view");
    view.update(cx, |v, cx| v.find_step(true, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.find_state().summary()),
        "2 of 2"
    );
    view.update(cx, |v, cx| v.find_step(false, cx));
    view.update(cx, |v, cx| v.find_step(false, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.find_state().summary()),
        "2 of 2"
    );
    view.update(cx, |v, cx| v.clear_find(cx));
    assert_eq!(view.read_with(cx, |v, _| v.find_state().summary()), "");
}

#[gpui::test]
fn dragging_selects_and_copy_takes_the_text(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", "alpha beta\ngamma delta\n", false);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let at = |col: f32, line: f32| {
        point(
            layout.text_left + layout.cell_width * col,
            layout.bounds.top() + layout.line_height * (line + 0.5),
        )
    };
    cx.simulate_mouse_down(at(6.0, 0.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(at(5.0, 1.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at(5.0, 1.0), MouseButton::Left, Modifiers::none());
    assert_eq!(
        view.read_with(cx, |v, cx| v.selected_text(cx)).as_deref(),
        Some("beta\ngamma")
    );
    // The crate installs no key bindings (the desktop does): copy through the action.
    cx.dispatch_action(hutan_code_view::Copy);
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("beta\ngamma")
    );
    assert_eq!(view.read_with(cx, |v, cx| v.position(cx)), (2, 6));
}
