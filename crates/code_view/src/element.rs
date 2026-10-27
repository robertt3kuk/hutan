//! The GPUI element that draws a [`CodeView`]: the gutter with line numbers, the lines that
//! intersect the view (and only those: a frame never lays out the whole file), syntax colors,
//! selections, carets, the input method's marked text, the matching bracket and find matches,
//! an agent's changes (E-10: a tint over the lines and a bar in the gutter, a mark where lines
//! were removed), diagnostics (E-12a: an underline whose style and a gutter glyph whose shape
//! say the severity, and the caret line's message after its end); the mouse; and the
//! platform's text input while the view has the keyboard.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, Focusable as _, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, ScrollWheelEvent, ShapedLine, SharedString, Size, Style, TextAlign,
    TextRun, Window, fill, outline, point, px, relative, size,
};
use hutan_syntax::{HighlightSpan, SyntaxView};

use crate::agent::HunkKind;
use crate::diagnostics::{Diagnostic, DrawnDiagnostic, GlyphShape, Severity, UnderlineStyle};
use crate::layout::DisplayLine;
use crate::style::{CodeStyle, rgba};
use crate::view::{CodeView, Layout, OVERSCAN_LINES, line_window};
use crate::wrap::{RowGeom, WrapMap};

/// How an agent's change marks a drawn line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct AgentMarks {
    /// The line is part of a change: tinted, with a bar of this color in the gutter.
    pub bar: Option<Hsla>,
    /// Lines were removed just above this one: a mark of this color on its top edge.
    pub removed_above: Option<Hsla>,
}

/// Longest blame label at the end of the caret line, in characters.
const INLINE_BLAME_CHARS: usize = 120;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

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
    /// Cells to add to a column of the line to get the cell it is drawn in, counted from the
    /// left of the text area: a wrapped row starts at its indent, whatever column it begins at.
    col_shift: f32,
    /// Column ranges painted behind the text: the current line, find matches, the current
    /// match, the matching brackets, the selections.
    fills: Vec<(Range<f32>, Hsla)>,
    /// Column ranges of the bar carets on this line (a block caret is one of the fills).
    carets: Vec<Range<f32>>,
    /// Column ranges of marked (composing) text, underlined.
    marked: Vec<Range<f32>>,
    /// An agent's change on or above the line.
    agent: AgentMarks,
    /// Diagnostics on the line: the columns underlined and how serious each is.
    underlines: Vec<(Range<f32>, Severity)>,
    /// The gutter glyph: the most serious diagnostic that starts on the line.
    glyph: Option<Severity>,
    /// The caret line's message, shaped, and the column it starts at.
    hint: Option<(Rc<ShapedLine>, f32)>,
    /// The blame of the line, on the first line of each run of one commit.
    blame: Option<Rc<ShapedLine>>,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    layout: Layout,
    gutter: Bounds<Pixels>,
    background: Hsla,
    gutter_background: Hsla,
    current_line: Option<Hsla>,
    /// The tint of lines an agent changed.
    agent_tint: Hsla,
    caret_color: Hsla,
    marked_color: Hsla,
    /// `editor.diagnostic.error|warning|info|hint`, in [`Severity`] order.
    diagnostic_colors: [Hsla; 4],
    scroll_x: Pixels,
    lines: Vec<LinePaint>,
    /// Where the gutter's blame text goes, when the column is on.
    blame: Option<Bounds<Pixels>>,
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
    let (
        text,
        ready,
        editable,
        syntax,
        hunks,
        diagnostics,
        diagnostics_epoch,
        has_blame,
        blame_cells,
        version,
    ) = {
        let buffer = view.buffer().read(cx);
        (
            buffer.text().clone(),
            buffer.is_ready(),
            buffer.is_editable(),
            buffer.syntax_view(),
            buffer.agent_hunks(),
            buffer.diagnostics(),
            buffer.diagnostics_epoch(),
            buffer.has_blame(),
            buffer.blame_cells(),
            buffer.version(),
        )
    };
    let blame_column = view.blame_gutter && has_blame;
    let blame_inline = view.inline_blame && has_blame;
    let now = now_secs();
    let line_count = if ready { text.line_count() } else { 0 };
    let digits = line_count.max(1).to_string().len().max(3);
    let previous_top = view.layout.map(|l| l.bounds.top());
    // `:set nonumber`: a narrow gutter without numbers. Either way it keeps room on its left
    // for an agent's bar and a diagnostic's glyph, so the text never moves when they come.
    let numbers = view.show_numbers;
    let gutter_w = if numbers {
        cell * (digits as f32 + 3.5)
    } else {
        cell * 2.0
    } + if blame_column {
        cell * blame_cells as f32
    } else {
        px(0.0)
    };
    // The blame sits right of the marks (an agent's bar, a diagnostic's glyph) and left of the
    // numbers.
    let blame_area = blame_column.then(|| {
        Bounds::new(
            point(bounds.left() + cell * 2.0, bounds.top()),
            size(cell * (blame_cells as f32 - 1.0), bounds.size.height),
        )
    });
    view.blame_area = blame_area;
    let text_left = bounds.left() + gutter_w + cell * 0.5;
    let rows = (f32::from(bounds.size.height) / f32::from(line_h)).ceil() as usize + 1;
    view.layout = Some(Layout {
        bounds,
        text_left,
        cell_width: cell,
        line_height: line_h,
        rows,
    });
    let buffer = view.buffer().clone();
    view.sync_wrap_frame(&text, version, |from| buffer.read(cx).edits_since(from));
    if std::mem::take(&mut view.keep_in_place)
        && let Some(previous) = previous_top
    {
        let moved_up = f32::from(previous - bounds.top());
        if moved_up > 0.0 {
            view.inset += moved_up;
            view.scroll_y -= moved_up;
        }
    }
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
        agent_tint: rgba(theme.agent_changed),
        caret_color: rgba(theme.cursor),
        marked_color: rgba(theme.foreground),
        diagnostic_colors: Severity::ALL.map(|s| style.diagnostic_color(s)),
        scroll_x: px(view.scroll_x),
        lines: Vec::new(),
        blame: blame_area,
        input: focused && editable,
    };
    view.stats.last_diagnostics.clear();
    view.stats.last_rows.clear();
    if line_count == 0 {
        view.stats.last_shaped = 0;
        view.stats.last_lines = 0..0;
        return prepaint;
    }

    let line_hf = f32::from(line_h);
    let first =
        ((view.scroll_y / line_hf).floor().max(0.0) as usize).saturating_sub(OVERSCAN_LINES);
    let offset = view.scroll_y - first as f32 * line_hf;
    // The rows in view: lines, or the rows the lines wrap to.
    let wrapped = view.wrap.borrow().map.is_some();
    let geoms: Vec<RowGeom> = {
        let wrap = view.wrap.borrow();
        let total = wrap.map.as_ref().map_or(line_count, WrapMap::total_rows);
        let shown = (rows + 2 * OVERSCAN_LINES).min(total - first.min(total));
        (first..first + shown)
            .map(|row| match &wrap.map {
                Some(map) => map.row_at(&text, row),
                None => RowGeom::whole_line(&text, row),
            })
            .collect()
    };
    let lines = geoms.first().map_or(0, |g| g.line)..geoms.last().map_or(0, |g| g.line + 1);
    let agent_marks = agent_marks(&text, &hunks, lines.clone(), theme);
    // The diagnostics that touch a line in view (they are sorted by where they start).
    let shown_diagnostics: Vec<&Diagnostic> = {
        let view_start = text.line_start(lines.start).unwrap_or(0);
        let view_end = text
            .line_range(lines.end.saturating_sub(1))
            .map_or(text.len(), |r| r.end);
        let past = diagnostics.partition_point(|d| d.bytes.start <= view_end);
        diagnostics[..past]
            .iter()
            .filter(|d| d.bytes.end >= view_start)
            .collect()
    };
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
    // A caret at the end of a wrapped row (`End`, a click past it) is drawn there, not at the
    // start of the row after it.
    let upstream_head = view.wrap.borrow().upstream.filter(|u| *u == primary.head);
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
    let mut rows: Vec<Row> = Vec::with_capacity(geoms.len());
    let mut missing: Option<Range<usize>> = None;
    for geom in geoms {
        let content = geom.bytes.clone();
        let (window_text, display) = if wrapped {
            let row_text = geom.text(&text);
            let end_col = crate::layout::line_columns_at(&row_text, geom.start_col);
            let display = DisplayLine::build_at(&row_text, geom.start_col, geom.start_col, end_col);
            (row_text, display)
        } else {
            let window_text = line_window(&text, content.clone(), to_col);
            let display = DisplayLine::build(&window_text, from_col, to_col);
            view.widest = view.widest.max(crate::layout::line_columns(&window_text));
            (window_text, display)
        };
        let key = hash_of((
            geom.line,
            geom.sub,
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
            missing = Some(missing.map_or(content.clone(), |m| m.start..content.end));
        }
        rows.push(Row {
            geom,
            window_text,
            display,
            key,
            text_line,
        });
    }
    if let Some(missing) = missing {
        let spans: Vec<HighlightSpan> = syntax
            .as_ref()
            .map(|s| s.highlights(missing))
            .unwrap_or_default();
        for row in rows
            .iter_mut()
            .filter(|r| r.text_line.is_none() && !r.display.text.is_empty())
        {
            let runs = runs_for(&row.display, row.geom.bytes.clone(), &spans, &style);
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
    for (row_index, row) in rows.into_iter().enumerate() {
        let Row {
            geom,
            window_text,
            display,
            text_line,
            ..
        } = row;
        let line = geom.line;
        let content = geom.bytes.clone();
        let base_col = geom.start_col;
        let last_row = geom.last;
        // Whether a range starting at `start` begins inside the row: the end of the line
        // belongs to its last row, the end of any other row to the row after it.
        let begins_in_row = |start: usize| {
            if last_row {
                start <= content.end
            } else {
                start < content.end
            }
        };

        let number_color = if head_line == Some(line) {
            theme.line_number_active
        } else {
            theme.gutter_foreground
        };
        let number_key = hash_of((line, view.style_generation, head_line == Some(line)));
        let number = if !numbers || !geom.first {
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
            crate::layout::col_of_byte_at(
                &window_text,
                base_col,
                byte.saturating_sub(content.start),
            ) as f32
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
            .take_while(|m| begins_in_row(m.start))
        {
            let clipped = found.start.max(line_range.start)..found.end.min(line_range.end);
            let color = if current_match.as_ref() == Some(found) {
                theme.search_match_active
            } else {
                theme.search_match
            };
            fills.push((columns(clipped, false), rgba(color)));
        }
        let mut selected_cells = 0usize;
        let first_selection = selections.partition_point(|s| s.end < line_range.start);
        for selected in selections[first_selection..]
            .iter()
            .take_while(|s| begins_in_row(s.start))
        {
            if selected.is_empty() {
                continue;
            }
            let clipped = selected.start.max(line_range.start)..selected.end.min(line_range.end);
            // The line break is selected too: one more cell.
            let eol = selected.end > line_range.end && last_row;
            if eol || !clipped.is_empty() {
                let cols = columns(clipped, eol);
                selected_cells += (cols.end - cols.start).round() as usize;
                fills.push((cols, selection_color));
            }
        }
        let carets: Vec<Range<f32>> = if focused {
            let first_head = heads.partition_point(|h| *h < line_range.start);
            heads[first_head..]
                .iter()
                .take_while(|h| **h <= line_range.end)
                .filter(|h| {
                    let at = **h;
                    if upstream_head == Some(at) {
                        // Drawn at the end of the row before the byte it stands at.
                        if !last_row && at == content.end {
                            return true;
                        }
                        if geom.sub > 0 && at == content.start {
                            return false;
                        }
                    }
                    begins_in_row(at)
                })
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

        // Diagnostics: the columns each covers on this row, the glyph of the most serious one
        // that starts on the line (beside its first row), and on the caret's line the most
        // serious one's message (after its last row).
        let mut underlines = Vec::new();
        for d in &shown_diagnostics {
            if !diagnostic_touches(d, &line_range, last_row) {
                continue;
            }
            let clipped = d.bytes.start.max(line_range.start)..d.bytes.end.min(line_range.end);
            underlines.push((columns(clipped, false), d.severity));
        }
        let mut glyph: Option<Severity> = None;
        let mut on_line: Option<&Diagnostic> = None;
        if geom.first || geom.last {
            let whole_line = if wrapped {
                text.line_content_range(line)
                    .unwrap_or_else(|_| line_range.clone())
            } else {
                line_range.clone()
            };
            for d in &shown_diagnostics {
                if geom.first
                    && d.bytes.start >= whole_line.start
                    && d.bytes.start <= whole_line.end
                    && glyph.is_none_or(|g| d.severity < g)
                {
                    glyph = Some(d.severity);
                }
                if geom.last
                    && diagnostic_touches(d, &whole_line, true)
                    && on_line.is_none_or(|o| d.severity < o.severity)
                {
                    on_line = Some(d);
                }
            }
        }
        for (index, (_, severity)) in underlines.iter().enumerate() {
            view.stats.last_diagnostics.push(DrawnDiagnostic {
                line,
                severity: *severity,
                underline: severity.underline(),
                glyph: (index == 0).then_some(glyph).flatten().map(Severity::glyph),
            });
        }
        let hint = match on_line {
            Some(d) if head_line == Some(line) => {
                let message: String = d.first_line().chars().take(160).collect();
                let symbol = d.severity.symbol();
                let label = format!("{symbol}  {message}");
                let key = hash_of(("hint", &label, d.severity, view.style_generation));
                let shaped = match view.numbers.get(key) {
                    Some(hit) => hit,
                    None => {
                        let runs = [
                            TextRun {
                                len: symbol.len(),
                                font: style.font(None),
                                color: style.diagnostic_color(d.severity),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            },
                            TextRun {
                                len: label.len() - symbol.len(),
                                font: style.font(None),
                                color: rgba(theme.gutter_foreground),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            },
                        ];
                        let shaped = Rc::new(text_system.shape_line(
                            SharedString::from(label),
                            px(style.font_size),
                            &runs,
                            None,
                        ));
                        view.numbers.insert(key, shaped.clone());
                        shaped
                    }
                };
                let end_col = crate::layout::line_columns_at(&window_text, base_col) as f32;
                Some((shaped, end_col + 3.0))
            }
            _ => None,
        };

        // Blame: the first line of a run of one commit says who, when and what; the caret line
        // may also say it after its end (unless it has a diagnostic's message there).
        let blame_text =
            |view: &CodeView, cx: &gpui::Context<CodeView>| -> (Option<String>, Option<String>) {
                let buffer = view.buffer().read(cx);
                let Some(this) = buffer.blame_line(line) else {
                    return (None, None);
                };
                let starts_run = line == 0
                    || buffer
                        .blame_line(line - 1)
                        .is_none_or(|before| !before.same_run(this));
                let label = crate::blame::label(this, now);
                let column = (blame_column && starts_run && geom.first)
                    .then(|| crate::blame::fit(&label, blame_cells - 3));
                let inline = (blame_inline && head_line == Some(line) && geom.last)
                    .then(|| crate::blame::fit(&label, INLINE_BLAME_CHARS));
                (column, inline)
            };
        let (column_label, inline_label) = if blame_column || blame_inline {
            blame_text(view, cx)
        } else {
            (None, None)
        };
        let blame = column_label.map(|label| {
            shape_muted(
                &mut view.numbers,
                &text_system,
                &style,
                (label, "blame", view.style_generation),
            )
        });
        let hint = hint.or_else(|| {
            inline_label.map(|label| {
                let shaped = shape_muted(
                    &mut view.numbers,
                    &text_system,
                    &style,
                    (format!("   {label}"), "inline-blame", view.style_generation),
                );
                let end_col = crate::layout::line_columns_at(&window_text, base_col) as f32;
                (shaped, end_col)
            })
        });

        view.stats.last_rows.push(crate::view::DrawnRow {
            line,
            sub: geom.sub,
            number: number.is_some(),
            carets: carets.len(),
            selected_cells,
        });
        let mut agent = agent_marks
            .get(line - lines.start)
            .copied()
            .unwrap_or_default();
        if !geom.first {
            agent.removed_above = None;
        }
        prepaint.lines.push(LinePaint {
            y: bounds.top() + px(row_index as f32 * line_hf - offset),
            number,
            text: text_line,
            start_col: display.start_col,
            col_shift: if wrapped {
                geom.indent as f32 - geom.start_col as f32
            } else {
                0.0
            },
            fills,
            carets: if block { Vec::new() } else { carets },
            marked,
            agent,
            underlines,
            glyph,
            hint,
            blame,
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
    if view
        .stats
        .diagnostics_drawn
        .is_none_or(|(epoch, _)| epoch != diagnostics_epoch)
    {
        view.stats.diagnostics_drawn = Some((diagnostics_epoch, view.stats.frames));
    }
    let now = Instant::now();
    view.times.first_text.get_or_insert(now);
    if highlighted {
        view.times.first_highlighted.get_or_insert(now);
    }
    prepaint
}

/// `text` shaped in the gutter's muted color, found again by `(text, tag, style generation)`.
fn shape_muted(
    cache: &mut crate::view::LineCache,
    text_system: &std::sync::Arc<gpui::WindowTextSystem>,
    style: &CodeStyle,
    (text, tag, generation): (String, &'static str, u64),
) -> Rc<ShapedLine> {
    let key = hash_of((tag, &text, generation));
    if let Some(hit) = cache.get(key) {
        return hit;
    }
    let run = TextRun {
        len: text.len(),
        font: style.font(None),
        color: rgba(style.theme.gutter_foreground),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = Rc::new(text_system.shape_line(
        SharedString::from(text),
        px(style.font_size),
        &[run],
        None,
    ));
    cache.insert(key, shaped.clone());
    shaped
}

/// The marks of an agent's changes (`hunks`, in byte order) on each line of `lines`.
fn agent_marks(
    text: &hutan_buffer::Text,
    hunks: &[crate::agent::AgentHunk],
    lines: Range<usize>,
    theme: &hutan_settings::themes::EditorTheme,
) -> Vec<AgentMarks> {
    let mut marks = vec![AgentMarks::default(); lines.len()];
    if hunks.is_empty() || lines.is_empty() {
        return marks;
    }
    let view_start = text.line_start(lines.start).unwrap_or(0);
    let view_end = text.line_range(lines.end - 1).map_or(text.len(), |r| r.end);
    let first = hunks.partition_point(|h| h.bytes.end < view_start);
    for hunk in hunks[first..]
        .iter()
        .take_while(|h| h.bytes.start <= view_end)
    {
        let start = hunk.bytes.start.min(text.len());
        let Ok(start_line) = text.line_of_byte(text.floor_char_boundary(start)) else {
            continue;
        };
        let kind = hunk.kind();
        if kind == HunkKind::Removed {
            if let Some(mark) = start_line
                .checked_sub(lines.start)
                .and_then(|i| marks.get_mut(i))
            {
                mark.removed_above = Some(rgba(theme.diff_removed));
            }
            continue;
        }
        // The hunk's bytes end after its last line break: that line is the last one it covers.
        let end = hunk.bytes.end.min(text.len()).saturating_sub(1).max(start);
        let end_line = text
            .line_of_byte(text.floor_char_boundary(end))
            .unwrap_or(start_line);
        let color = rgba(if kind == HunkKind::Added {
            theme.diff_added
        } else {
            theme.diff_modified
        });
        for line in start_line.max(lines.start)..=end_line.min(lines.end - 1) {
            marks[line - lines.start].bar = Some(color);
        }
    }
    marks
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
    let x_of = |line: &LinePaint, col: f32| {
        layout.text_left + cell * (col + line.col_shift) - state.scroll_x
    };

    window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
        // An agent's changed lines: tinted under everything else, across the text area.
        for line in state.lines.iter().filter(|l| l.agent.bar.is_some()) {
            window.paint_quad(fill(
                Bounds::from_corners(
                    point(text_area.left(), line.y),
                    point(text_area.right(), line.y + line_h),
                ),
                state.agent_tint,
            ));
        }
        for line in &state.lines {
            for (cols, color) in &line.fills {
                let (left, right, color) = if cols.start == f32::NEG_INFINITY {
                    // The current line.
                    let Some(color) = state.current_line else {
                        continue;
                    };
                    (text_area.left(), text_area.right(), color)
                } else {
                    (x_of(line, cols.start), x_of(line, cols.end), *color)
                };
                window.paint_quad(fill(
                    Bounds::from_corners(point(left, line.y), point(right, line.y + line_h)),
                    color,
                ));
            }
        }
        for line in &state.lines {
            if let Some(text) = &line.text {
                let origin = point(x_of(line, line.start_col as f32), line.y);
                let _ = text.paint(origin, line_h, TextAlign::Left, None, window, cx);
            }
            for marked in &line.marked {
                let left = x_of(line, marked.start);
                let right = x_of(line, marked.end.max(marked.start + 1.0));
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(left, line.y + line_h - px(2.0)),
                        point(right, line.y + line_h - px(1.0)),
                    ),
                    state.marked_color,
                ));
            }
            for caret in &line.carets {
                let x = x_of(line, caret.start);
                window.paint_quad(fill(
                    Bounds::new(point(x - px(1.0), line.y), size(px(2.0), line_h)),
                    state.caret_color,
                ));
            }
            // Diagnostics: the style of the line says the severity as well as its color.
            for (cols, severity) in &line.underlines {
                let color = state.diagnostic_colors[*severity as usize];
                let left = x_of(line, cols.start);
                let right = x_of(line, cols.end.max(cols.start + 1.0));
                paint_underline(
                    window,
                    severity.underline(),
                    left,
                    right,
                    line.y + line_h,
                    color,
                );
            }
            if let Some((hint, col)) = &line.hint {
                let origin = point(x_of(line, *col), line.y);
                let _ = hint.paint(origin, line_h, TextAlign::Left, None, window, cx);
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
    if let Some(area) = state.blame {
        window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
            for line in &state.lines {
                if let Some(blame) = &line.blame {
                    let origin = point(area.left(), line.y);
                    let _ = blame.paint(origin, line_h, TextAlign::Left, None, window, cx);
                }
            }
        });
    }
    // The gutter's marks of an agent's changes: a bar beside changed lines, a wedge where lines
    // were removed.
    let bar_left = state.gutter.left() + px(1.0);
    for line in &state.lines {
        if let Some(color) = line.agent.bar {
            window.paint_quad(fill(
                Bounds::new(point(bar_left, line.y), size(px(3.0), line_h)),
                color,
            ));
        }
        if let Some(color) = line.agent.removed_above {
            window.paint_quad(fill(
                Bounds::new(point(bar_left, line.y - px(1.5)), size(cell, px(3.0))),
                color,
            ));
        }
    }
    // Diagnostic glyphs, right of the agent's bar: a shape per severity.
    let glyph_size = (cell * 0.85).min(line_h * 0.6);
    let glyph_left = state.gutter.left() + px(6.0);
    for line in &state.lines {
        if let Some(severity) = line.glyph {
            let top = line.y + (line_h - glyph_size) / 2.0;
            let bounds = Bounds::new(point(glyph_left, top), size(glyph_size, glyph_size));
            paint_glyph(
                window,
                severity.glyph(),
                bounds,
                state.diagnostic_colors[severity as usize],
            );
        }
    }
}

/// An underline from `left` to `right` just above `bottom`: a wave, a row of dots or a thin
/// line.
fn paint_underline(
    window: &mut Window,
    style: UnderlineStyle,
    left: Pixels,
    right: Pixels,
    bottom: Pixels,
    color: Hsla,
) {
    match style {
        UnderlineStyle::Wavy => window.paint_underline(
            point(left, bottom - px(3.5)),
            right - left,
            &gpui::UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: true,
            },
        ),
        UnderlineStyle::Dotted => {
            let dot = px(1.5);
            let mut x = left;
            while x + dot <= right {
                window.paint_quad(
                    fill(
                        Bounds::new(point(x, bottom - px(2.5)), size(dot, dot)),
                        color,
                    )
                    .corner_radii(dot / 2.0),
                );
                x += dot * 2.5;
            }
        }
        UnderlineStyle::Straight => window.paint_quad(fill(
            Bounds::from_corners(
                point(left, bottom - px(2.0)),
                point(right, bottom - px(1.0)),
            ),
            color,
        )),
    }
}

/// A diagnostic's gutter glyph in `bounds`: a disc, a triangle, a diamond or a ring.
fn paint_glyph(window: &mut Window, shape: GlyphShape, bounds: Bounds<Pixels>, color: Hsla) {
    let (l, t, r, b) = (bounds.left(), bounds.top(), bounds.right(), bounds.bottom());
    let mid_x = l + (r - l) / 2.0;
    let mid_y = t + (b - t) / 2.0;
    match shape {
        GlyphShape::Disc => {
            window.paint_quad(fill(bounds, color).corner_radii(bounds.size.width / 2.0));
        }
        GlyphShape::Ring => window.paint_quad(
            outline(bounds, color, BorderStyle::Solid)
                .border_widths(px(1.5))
                .corner_radii(bounds.size.width / 2.0),
        ),
        GlyphShape::Triangle | GlyphShape::Diamond => {
            let mut path = PathBuilder::fill();
            if shape == GlyphShape::Triangle {
                path.move_to(point(mid_x, t));
                path.line_to(point(r, b));
                path.line_to(point(l, b));
            } else {
                path.move_to(point(mid_x, t));
                path.line_to(point(r, mid_y));
                path.line_to(point(mid_x, b));
                path.line_to(point(l, mid_y));
            }
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
}

/// One row of a frame: what it shows and its shaped text, if it is already shaped.
struct Row {
    geom: RowGeom,
    window_text: String,
    display: DisplayLine,
    key: u64,
    text_line: Option<Rc<ShapedLine>>,
}

/// Whether diagnostic `d` is drawn on the row of `range` (the end of a line belongs to its last
/// row, the end of any other row to the row after it).
fn diagnostic_touches(d: &Diagnostic, range: &Range<usize>, last_row: bool) -> bool {
    let starts_before_end = if last_row {
        d.bytes.start <= range.end
    } else {
        d.bytes.start < range.end
    };
    if d.bytes.is_empty() || range.is_empty() {
        starts_before_end && d.bytes.end >= range.start
    } else {
        starts_before_end && d.bytes.end > range.start
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
