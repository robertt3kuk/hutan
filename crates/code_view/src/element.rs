//! The GPUI element that draws a [`CodeView`]: the gutter with line numbers, the lines that
//! intersect the view (and only those: a frame never lays out the whole file), syntax colors,
//! selection and find matches; and the mouse.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, Entity,
    Focusable as _, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, ShapedLine,
    SharedString, Size, Style, TextAlign, TextRun, Window, fill, point, px, relative, size,
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
    /// Column ranges painted behind the text: find matches, the current match, the selection.
    fills: Vec<(Range<f32>, Hsla)>,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    layout: Layout,
    gutter: Bounds<Pixels>,
    background: Hsla,
    gutter_background: Hsla,
    scroll_x: Pixels,
    lines: Vec<LinePaint>,
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

fn prepare(
    view: &mut CodeView,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
    cx: &mut gpui::Context<CodeView>,
) -> Prepaint {
    let style = view.style.clone();
    let (cell, line_h) = metrics(&style, window);
    let (text, version, ready, syntax) = {
        let buffer = view.buffer().read(cx);
        (
            buffer.text().clone(),
            buffer.version(),
            buffer.is_ready(),
            buffer.syntax_view(),
        )
    };
    let line_count = if ready { text.line_count() } else { 0 };
    let digits = line_count.max(1).to_string().len().max(3);
    let gutter_w = cell * (digits as f32 + 2.5);
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
    view.clamp_scroll(cx);
    view.lines.begin_frame();
    view.numbers.begin_frame();
    view.stats.frames += 1;
    view.note_drawn(version);

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
        scroll_x: px(view.scroll_x),
        lines: Vec::new(),
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

    let selection = view.selection.filter(|s| !s.is_empty()).map(|s| s.range());
    let selection_color = if view.focus_handle(cx).is_focused(window) {
        rgba(theme.selection)
    } else {
        rgba(theme.selection_inactive)
    };
    let head_line = view.selection.and_then(|s| text.line_of_byte(s.head).ok());
    let find_matches = &view.find.result.matches;
    let current_match = view.find.current_match();

    // Highlights are asked for once per frame, and only when a line has to be shaped.
    let mut spans: Option<Vec<HighlightSpan>> = None;
    let mut shaped = 0usize;
    let text_system = window.text_system().clone();
    for line in lines.clone() {
        let Ok(content) = text.line_content_range(line) else {
            continue;
        };
        let window_text = line_window(&text, content.clone(), to_col);
        let display = DisplayLine::build(&window_text, from_col, to_col);
        view.widest = view.widest.max(crate::layout::line_columns(&window_text));
        let key = {
            let mut h = DefaultHasher::new();
            (
                line,
                version,
                from_col,
                to_col,
                view.style_generation,
                syntax_key,
            )
                .hash(&mut h);
            h.finish()
        };
        let text_line = if display.text.is_empty() {
            None
        } else if let Some(hit) = view.lines.get(key) {
            Some(hit)
        } else {
            let spans = spans.get_or_insert_with(|| {
                let start = text.line_start(lines.start).unwrap_or(0);
                let end = text
                    .line_range(lines.end.saturating_sub(1))
                    .map_or(text.len(), |r| r.end);
                syntax
                    .as_ref()
                    .map(|s| s.highlights(start..end))
                    .unwrap_or_default()
            });
            let runs = runs_for(&display, content.clone(), spans, &style);
            let shaped_line = Rc::new(text_system.shape_line(
                SharedString::from(display.text.clone()),
                px(style.font_size),
                &runs,
                Some(cell),
            ));
            shaped += 1;
            view.lines.insert(key, shaped_line.clone());
            Some(shaped_line)
        };

        let number_color = if head_line == Some(line) {
            theme.line_number_active
        } else {
            theme.gutter_foreground
        };
        let number_key = {
            let mut h = DefaultHasher::new();
            (line, view.style_generation, head_line == Some(line)).hash(&mut h);
            h.finish()
        };
        let number = match view.numbers.get(number_key) {
            Some(hit) => hit,
            None => {
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
                shaped_number
            }
        };

        let mut fills = Vec::new();
        let line_range = content.start..content.end;
        let columns = |range: Range<usize>, extend_eol: bool| -> Range<f32> {
            let from = crate::layout::col_of_byte(&window_text, range.start - content.start);
            let to = crate::layout::col_of_byte(&window_text, range.end - content.start);
            let to = if extend_eol { to + 1 } else { to.max(from + 1) };
            from as f32..to as f32
        };
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
        if let Some(selected) = &selection
            && selected.start <= line_range.end
            && selected.end > line_range.start
        {
            let clipped = selected.start.max(line_range.start)..selected.end.min(line_range.end);
            // The line break is selected too: one more cell.
            let eol = selected.end > line_range.end;
            if eol || !clipped.is_empty() {
                fills.push((columns(clipped, eol), selection_color));
            }
        }

        prepaint.lines.push(LinePaint {
            y: bounds.top() + px((line - first) as f32 * line_hf - offset),
            number: Some(number),
            text: text_line,
            start_col: display.start_col,
            fills,
        });
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
        }
        if shown.end > shown.start.max(at) {
            runs.push(run(shown.end - shown.start.max(at), Some(span.token)));
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
                let left = x_of(cols.start);
                let right = x_of(cols.end);
                window.paint_quad(fill(
                    Bounds::from_corners(point(left, line.y), point(right, line.y + line_h)),
                    *color,
                ));
            }
        }
        for line in &state.lines {
            if let Some(text) = &line.text {
                let origin = point(x_of(line.start_col as f32), line.y);
                let _ = text.paint(origin, line_h, TextAlign::Left, None, window, cx);
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
