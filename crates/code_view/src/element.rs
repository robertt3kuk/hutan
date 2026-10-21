//! The GPUI element that draws a [`CodeView`]: the gutter with line numbers, the lines that
//! intersect the view (and only those: a frame never lays out the whole file), syntax colors,
//! selections, carets, the input method's marked text, the matching bracket and find matches;
//! the mouse; and the platform's text input while the view has the keyboard.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, Focusable as _, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent,
    ShapedLine, SharedString, Size, Style, TextAlign, TextRun, Window, fill, point, px, relative,
    size,
};
use hutan_syntax::{HighlightSpan, SyntaxView};

use crate::layout::DisplayLine;
use crate::style::{CodeStyle, rgba};
use crate::view::{CodeView, Layout, OVERSCAN_LINES, line_window};

/// Columns drawn past the right edge (and before the left one) of a horizontally scrolled view.
const MARGIN_COLS: usize = 8;

pub(crate) struct CodeElement {
    view: Entity<CodeView>,
}

impl CodeElement {
    pub fn new(view: Entity<CodeView>) -> Self {
        Self { view }
    }
}

impl IntoElement for CodeElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// One drawn line.
struct LinePaint {
    /// Top of the line.
    y: Pixels,
    number: Option<Rc<ShapedLine>>,
    text: Option<Rc<ShapedLine>>,
    /// Column the shaped text starts at.
    start_col: usize,
    /// Column ranges painted behind the text: the current line, find matches, the current
    /// match, the matching brackets, the selections.
    fills: Vec<(Range<f32>, Hsla)>,
    /// Column ranges of the bar carets on this line (a block caret is one of the fills).
    carets: Vec<Range<f32>>,
    /// Column ranges of marked (composing) text, underlined.
    marked: Vec<Range<f32>>,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    layout: Layout,
    gutter: Bounds<Pixels>,
    background: Hsla,
    gutter_background: Hsla,
    current_line: Option<Hsla>,
    caret_color: Hsla,
    marked_color: Hsla,
    scroll_x: Pixels,
    lines: Vec<LinePaint>,
    /// Whether the platform's text input goes to the view (it has the keyboard and may edit).
    input: bool,
}

impl Element for CodeElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: Size {
                width: relative(1.).into(),
                height: relative(1.).into(),
            },
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.view
            .update(cx, |view, cx| prepare(view, bounds, hitbox, window, cx))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if state.input {
            let focus = self.view.read(cx).focus_handle(cx);
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.view.clone()),
                cx,
            );
        }
        window.set_cursor_style(CursorStyle::IBeam, &state.hitbox);
        register_mouse_listeners(&self.view, bounds, window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint(state, window, cx);
        });
    }
}

/// Font metrics of the style: the cell (the advance of `M`) and the line height.
fn metrics(style: &CodeStyle, window: &Window) -> (Pixels, Pixels) {
    let size = px(style.font_size);
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&style.font(None));
    let cell = text_system
        .advance(font_id, size, 'M')
        .ok()
        .map(|a| a.width)
        .filter(|w| *w > px(0.))
        .unwrap_or(size * 0.6);
    (cell, (size * style.line_height).round())
}

fn hash_of(value: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// What a shaped line shows: its text and the color, weight and slant of every run.
fn content_key(text: &str, runs: &[TextRun], style_generation: u64) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    style_generation.hash(&mut h);
    for run in runs {
        run.len.hash(&mut h);
        for part in [run.color.h, run.color.s, run.color.l, run.color.a] {
            part.to_bits().hash(&mut h);
        }
        run.font.weight.0.to_bits().hash(&mut h);
        (run.font.style as u8).hash(&mut h);
    }
    h.finish()
}

fn prepare(
    view: &mut CodeView,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    cx: &mut gpui::Context<CodeView>,
) -> Prepaint {
    let style = view.style.clone();
    let (cell, line_h) = metrics(&style, window);
    let (text, ready, editable, syntax) = {
        let buffer = view.buffer().read(cx);
        (
            buffer.text().clone(),
            buffer.is_ready(),
            buffer.is_editable(),
            buffer.syntax_view(),
        )
    };
    let line_count = if ready { text.line_count() } else { 0 };
    let digits = line_count.max(1).to_string().len().max(3);
    // `:set nonumber`: a narrow gutter without numbers.
    let numbers = view.show_numbers;
    let gutter_w = if numbers {
        cell * (digits as f32 + 2.5)
    } else {
        cell * 1.0
    };
    let text_left = bounds.left() + gutter_w + cell * 0.5;
    let rows = (f32::from(bounds.size.height) / f32::from(line_h)).ceil() as usize + 1;
    view.layout = Some(Layout {
        bounds,
        text_left,
        cell_width: cell,
        line_height: line_h,
        rows,
    });
    view.apply_reveal(cx);
    if view.autoscroll {
        view.reveal_head(cx);
    }
    view.clamp_scroll(cx);
    view.lines.begin_frame();
    view.shapes.begin_frame();
    view.numbers.begin_frame();
    view.stats.frames += 1;

    let focused = view.focus_handle(cx).is_focused(window);
    let theme = &style.theme;
    let mut prepaint = Prepaint {
        hitbox,
        layout: view.layout.unwrap_or(Layout {
            bounds,
            text_left,
            cell_width: cell,
            line_height: line_h,
            rows,
        }),
        gutter: Bounds::new(bounds.origin, size(gutter_w, bounds.size.height)),
        background: rgba(theme.background),
        gutter_background: rgba(theme.gutter_background),
        current_line: None,
        caret_color: rgba(theme.cursor),
        marked_color: rgba(theme.foreground),
        scroll_x: px(view.scroll_x),
        lines: Vec::new(),
        input: focused && editable,
    };
    if line_count == 0 {
        view.stats.last_shaped = 0;
        view.stats.last_lines = 0..0;
        return prepaint;
    }

    let line_hf = f32::from(line_h);
    let first = ((view.scroll_y / line_hf).floor() as usize).saturating_sub(OVERSCAN_LINES);
    let offset = view.scroll_y - first as f32 * line_hf;
    let shown = (rows + 2 * OVERSCAN_LINES).min(line_count - first.min(line_count));
    let lines = first..first + shown;
    let visible_w = f32::from(bounds.right() - text_left).max(0.0);
    let from_col = ((view.scroll_x / f32::from(cell)).floor() as usize).saturating_sub(MARGIN_COLS);
    let to_col = from_col + (visible_w / f32::from(cell)).ceil() as usize + 2 * MARGIN_COLS;
    let syntax_key = syntax_key(syntax.as_ref());
    let highlighted = syntax.as_ref().is_some_and(SyntaxView::is_current);

    let selection_color = if focused {
        rgba(theme.selection)
    } else {
        rgba(theme.selection_inactive)
    };
    let selections: Vec<Range<usize>> = view.drawn_selections();
    let heads: Vec<usize> = {
        let mut heads: Vec<usize> = view.selections.iter().map(|s| s.head).collect();
        heads.sort_unstable();
        heads
    };
    let primary = view.selections.primary();
    let head_line = text.line_of_byte(primary.head.min(text.len())).ok();
    // Vim's Normal and Visual draw a block over the character under the caret.
    let block = view.block_caret();
    // The current line is tinted while there is one caret and nothing is selected.
    if focused && view.selections.len() == 1 && selections.iter().all(|s| s.is_empty()) {
        prepaint.current_line = Some(rgba(theme.current_line));
    }
    let brackets = if focused && primary.is_empty() && highlighted {
        syntax
            .as_ref()
            .and_then(|s| s.snapshot().bracket_match(primary.head))
    } else {
        None
    };
    let find_matches = &view.find.result.matches;
    let current_match = view.find.current_match();

    // What each line shows. A shaped line is found again by its line, its text and the source
    // of its colors (the tree, mapped across edits until a new one lands), not by the text's
    // version: after a keystroke only the line it changed misses, and highlights are asked for
    // the lines that miss, not for the whole view.
    let mut shaped = 0usize;
    let text_system = window.text_system().clone();
    let mut rows: Vec<Row> = Vec::with_capacity(lines.len());
    let mut missing: Option<Range<usize>> = None;
    for line in lines.clone() {
        let Ok(content) = text.line_content_range(line) else {
            continue;
        };
        let window_text = line_window(&text, content.clone(), to_col);
        let display = DisplayLine::build(&window_text, from_col, to_col);
        view.widest = view.widest.max(crate::layout::line_columns(&window_text));
        let key = hash_of((
            line,
            hash_of(&display.text),
            display.start_col,
            from_col,
            to_col,
            view.style_generation,
            syntax_key,
        ));
        let text_line = if display.text.is_empty() {
            None
        } else {
            view.lines.get(key)
        };
        if text_line.is_none() && !display.text.is_empty() {
            missing = Some(missing.map_or(line..line + 1, |m| m.start..line + 1));
        }
        rows.push(Row {
            line,
            content,
            window_text,
            display,
            key,
            text_line,
        });
    }
    if let Some(missing) = missing {
        let start = text.line_start(missing.start).unwrap_or(0);
        let end = text
            .line_range(missing.end - 1)
            .map_or(text.len(), |r| r.end);
        let spans: Vec<HighlightSpan> = syntax
            .as_ref()
            .map(|s| s.highlights(start..end))
            .unwrap_or_default();
        for row in rows
            .iter_mut()
            .filter(|r| r.text_line.is_none() && !r.display.text.is_empty())
        {
            let runs = runs_for(&row.display, row.content.clone(), &spans, &style);
            let shown = content_key(&row.display.text, &runs, view.style_generation);
            let shaped_line = match view.shapes.get(shown) {
                Some(hit) => hit,
                None => {
                    let shaped_line = Rc::new(text_system.shape_line(
                        SharedString::from(row.display.text.clone()),
                        px(style.font_size),
                        &runs,
                        Some(cell),
                    ));
                    shaped += 1;
                    view.shapes.insert(shown, shaped_line.clone());
                    shaped_line
                }
            };
            view.lines.insert(row.key, shaped_line.clone());
            row.text_line = Some(shaped_line);
        }
    }
    for row in rows {
        let Row {
            line,
            content,
            window_text,
            display,
            text_line,
            ..
        } = row;

        let number_color = if head_line == Some(line) {
            theme.line_number_active
        } else {
            theme.gutter_foreground
        };
        let number_key = hash_of((line, view.style_generation, head_line == Some(line)));
        let number = if !numbers {
            None
        } else if let Some(hit) = view.numbers.get(number_key) {
            Some(hit)
        } else {
            let label = (line + 1).to_string();
            let run = TextRun {
                len: label.len(),
                font: style.font(None),
                color: rgba(number_color),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped_number = Rc::new(text_system.shape_line(
                SharedString::from(label),
                px(style.font_size),
                &[run],
                Some(cell),
            ));
            view.numbers.insert(number_key, shaped_number.clone());
            Some(shaped_number)
        };

        let line_range = content.start..content.end;
        let col = |byte: usize| -> f32 {
            crate::layout::col_of_byte(&window_text, byte.saturating_sub(content.start)) as f32
        };
        let columns = |range: Range<usize>, extend_eol: bool| -> Range<f32> {
            let from = col(range.start);
            let to = col(range.end);
            let to = if extend_eol {
                to + 1.0
            } else {
                to.max(from + 1.0)
            };
            from..to
        };
        let mut fills = Vec::new();
        if let Some(pair) = &brackets {
            for bracket in [&pair.open, &pair.close] {
                if bracket.start >= line_range.start && bracket.end <= line_range.end {
                    fills.push((columns(bracket.clone(), false), rgba(theme.bracket_match)));
                }
            }
        }
        let first_match = find_matches.partition_point(|m| m.end <= line_range.start);
        for found in find_matches[first_match..]
            .iter()
            .take_while(|m| m.start <= line_range.end)
        {
            let clipped = found.start.max(line_range.start)..found.end.min(line_range.end);
            let color = if current_match.as_ref() == Some(found) {
                theme.search_match_active
            } else {
                theme.search_match
            };
            fills.push((columns(clipped, false), rgba(color)));
        }
        let first_selection = selections.partition_point(|s| s.end < line_range.start);
        for selected in selections[first_selection..]
            .iter()
            .take_while(|s| s.start <= line_range.end)
        {
            if selected.is_empty() {
                continue;
            }
            let clipped = selected.start.max(line_range.start)..selected.end.min(line_range.end);
            // The line break is selected too: one more cell.
            let eol = selected.end > line_range.end;
            if eol || !clipped.is_empty() {
                fills.push((columns(clipped, eol), selection_color));
            }
        }
        let carets: Vec<Range<f32>> = if focused {
            let first_head = heads.partition_point(|h| *h < line_range.start);
            heads[first_head..]
                .iter()
                .take_while(|h| **h <= line_range.end)
                .map(|h| {
                    let from = col(*h);
                    // A block covers the character's cells (one at the end of the line).
                    let to = match text.char_at(*h).filter(|c| *c != '\n') {
                        Some(c) if block => col(*h + c.len_utf8()).max(from + 1.0),
                        _ => from + 1.0,
                    };
                    from..to
                })
                .collect()
        } else {
            Vec::new()
        };
        if block {
            let mut color = rgba(theme.cursor);
            color.a *= 0.55;
            fills.extend(carets.iter().map(|c| (c.clone(), color)));
        }
        let marked = view
            .marked
            .iter()
            .filter(|m| m.start < line_range.end.max(m.start + 1) && m.end > line_range.start)
            .map(|m| col(m.start.max(line_range.start))..col(m.end.min(line_range.end)))
            .collect();

        prepaint.lines.push(LinePaint {
            y: bounds.top() + px((line - first) as f32 * line_hf - offset),
            number,
            text: text_line,
            start_col: display.start_col,
            fills,
            carets: if block { Vec::new() } else { carets },
            marked,
        });
        if prepaint.current_line.is_some()
            && head_line == Some(line)
            && let Some(last) = prepaint.lines.last_mut()
        {
            // Behind everything else on the line, across the whole text area.
            last.fills
                .insert(0, (f32::NEG_INFINITY..f32::INFINITY, Hsla::default()));
        }
    }
    view.stats.last_shaped = shaped;
    view.stats.lines_shaped += shaped as u64;
    view.stats.last_lines = lines;
    let now = Instant::now();
    view.times.first_text.get_or_insert(now);
    if highlighted {
        view.times.first_highlighted.get_or_insert(now);
    }
    prepaint
}

/// What identifies the syntax a frame colors with: the version of the tree and whether it is
/// mapped across edits.
fn syntax_key(syntax: Option<&SyntaxView>) -> (u64, bool) {
    syntax.map_or((u64::MAX, false), |s| {
        (s.snapshot().version(), s.is_current())
    })
}

/// Text runs of `display` (a line starting at byte `line_start` of the file) colored by `spans`.
fn runs_for(
    display: &DisplayLine,
    line: Range<usize>,
    spans: &[HighlightSpan],
    style: &CodeStyle,
) -> Vec<TextRun> {
    let run = |len: usize, token| TextRun {
        len,
        font: style.font(token),
        color: style.token_color(token),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0usize;
    let first = spans.partition_point(|s| s.range.end <= line.start);
    for span in spans[first..]
        .iter()
        .take_while(|s| s.range.start < line.end)
    {
        let rel =
            span.range.start.saturating_sub(line.start)..span.range.end.saturating_sub(line.start);
        let shown = display.text_range(rel);
        if shown.start > at {
            runs.push(run(shown.start - at, None));
            at = shown.start;
        }
        if shown.end > at {
            runs.push(run(shown.end - at, Some(span.token)));
            at = shown.end;
        }
    }
    if display.text.len() > at {
        runs.push(run(display.text.len() - at, None));
    }
    runs
}

fn register_mouse_listeners(view: &Entity<CodeView>, bounds: Bounds<Pixels>, window: &mut Window) {
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && bounds.contains(&event.position) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_move(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &ScrollWheelEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && bounds.contains(&event.position) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
            }
        }
    });
}

fn paint(state: &Prepaint, window: &mut Window, cx: &mut App) {
    let layout = state.layout;
    let bounds = layout.bounds;
    window.paint_quad(fill(bounds, state.background));
    window.paint_quad(fill(state.gutter, state.gutter_background));
    let cell = layout.cell_width;
    let line_h = layout.line_height;
    let text_area = Bounds::from_corners(
        point(layout.text_left - cell * 0.25, bounds.top()),
        bounds.bottom_right(),
    );
    let x_of = |col: f32| layout.text_left + cell * col - state.scroll_x;

    window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
        for line in &state.lines {
            for (cols, color) in &line.fills {
                let (left, right, color) = if cols.start == f32::NEG_INFINITY {
                    // The current line.
                    let Some(color) = state.current_line else {
                        continue;
                    };
                    (text_area.left(), text_area.right(), color)
                } else {
                    (x_of(cols.start), x_of(cols.end), *color)
                };
                window.paint_quad(fill(
                    Bounds::from_corners(point(left, line.y), point(right, line.y + line_h)),
                    color,
                ));
            }
        }
        for line in &state.lines {
            if let Some(text) = &line.text {
                let origin = point(x_of(line.start_col as f32), line.y);
                let _ = text.paint(origin, line_h, TextAlign::Left, None, window, cx);
            }
            for marked in &line.marked {
                let left = x_of(marked.start);
                let right = x_of(marked.end.max(marked.start + 1.0));
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(left, line.y + line_h - px(2.0)),
                        point(right, line.y + line_h - px(1.0)),
                    ),
                    state.marked_color,
                ));
            }
            for caret in &line.carets {
                let x = x_of(caret.start);
                window.paint_quad(fill(
                    Bounds::new(point(x - px(1.0), line.y), size(px(2.0), line_h)),
                    state.caret_color,
                ));
            }
        }
    });

    let number_right = state.gutter.right() - cell;
    for line in &state.lines {
        if let Some(number) = &line.number {
            let origin = point(number_right - number.width, line.y);
            let _ = number.paint(origin, line_h, TextAlign::Left, None, window, cx);
        }
    }
}

/// One line of a frame: what it shows and its shaped text, if it is already shaped.
struct Row {
    line: usize,
    content: Range<usize>,
    window_text: String,
    display: DisplayLine,
    key: u64,
    text_line: Option<Rc<ShapedLine>>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use hutan_settings::themes::{Builtin, builtin_theme};
    use hutan_syntax::Token;

    fn style() -> CodeStyle {
        CodeStyle::new(
            builtin_theme(Builtin::HutanDark).editor(),
            "Menlo",
            13.0,
            1.4,
        )
    }

    #[test]
    fn runs_cover_the_line_and_color_the_spans() {
        let display = DisplayLine::build("fn main() {}", 0, 80);
        let spans = vec![
            HighlightSpan {
                range: 100..102,
                token: Token::Keyword,
            },
            HighlightSpan {
                range: 103..107,
                token: Token::Function,
            },
        ];
        let style = style();
        let runs = runs_for(&display, 100..112, &spans, &style);
        let lens: Vec<usize> = runs.iter().map(|r| r.len).collect();
        assert_eq!(lens, vec![2, 1, 4, 5]);
        assert_eq!(
            runs.iter().map(|r| r.len).sum::<usize>(),
            display.text.len()
        );
        assert_eq!(runs[0].color, style.token_color(Some(Token::Keyword)));
        assert_eq!(runs[1].color, style.token_color(None));
    }

    #[test]
    fn spans_right_of_the_window_never_overrun_the_text() {
        // Columns 0..5 of a long line; the span lies past the window, the second one inside.
        let display = DisplayLine::build("0123456789abcdef", 0, 5);
        let spans = vec![
            HighlightSpan {
                range: 1..2,
                token: Token::Keyword,
            },
            HighlightSpan {
                range: 10..12,
                token: Token::String,
            },
            HighlightSpan {
                range: 13..14,
                token: Token::Number,
            },
        ];
        let runs = runs_for(&display, 0..16, &spans, &style());
        assert_eq!(
            runs.iter().map(|r| r.len).sum::<usize>(),
            display.text.len()
        );
    }

    #[test]
    fn spans_of_other_lines_are_ignored() {
        let display = DisplayLine::build("abc", 0, 80);
        let spans = vec![
            HighlightSpan {
                range: 0..5,
                token: Token::Comment,
            },
            HighlightSpan {
                range: 50..60,
                token: Token::String,
            },
        ];
        let runs = runs_for(&display, 10..13, &spans, &style());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 3);
    }
}
