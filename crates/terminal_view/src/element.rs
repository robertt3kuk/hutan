//! The GPUI element that paints the terminal grid and wires mouse, input-method and ack handling.

use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, ShapedLine, Size, Style, TextAlign, Window, WindowTextSystem, fill, outline,
    point, px, relative, size,
};
use hutan_protocol::render::{CellFlags, Color, CursorShape, Row, Run};
use hutan_settings::themes::derive_block_fills;

use crate::blocks::{self, DisplayRow, OverlayReport, Tone};
use crate::model::{row_cells, row_width};
use crate::settings::Tint;
use crate::shape::{
    Decoration, FontSpec, RowCache, RowHover, ShapedRow, UnderlineKind, hsla, row_key, shape_row,
    styled_row, text_columns,
};
use crate::theme::{Rgb, Theme};
use crate::view::{ANIMATION_PHASES, Layout, TerminalView, ViewMode};

pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>) -> Self {
        Self { view }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct RowPaint {
    shaped: Option<Rc<ShapedRow>>,
    /// Selected columns, end exclusive.
    selected: Option<(u16, u16)>,
    /// The selected cells' text in the pack's selection foreground, painted over the row's own.
    selected_text: Option<SelectionText>,
    /// Fill of the whole row (the summary row of a folded block).
    fill: Option<Hsla>,
}

/// The text of the selected cells of one row, recolored.
struct SelectionText {
    /// First column, and one past the last column, of the cells it covers.
    from: u16,
    to: u16,
    shaped: ShapedRow,
}

/// The block overlay's tints for the colors a terminal is showing now.
struct Fills {
    separator: Hsla,
    highlight: Hsla,
    hover: Hsla,
    folded: Hsla,
}

fn tint_hsla(tint: Tint) -> Hsla {
    hsla(tint.rgb).opacity(tint.alpha)
}

/// The pack's declared fills, else the registry fallbacks evaluated on the live colors, so an
/// application that sets its own background (OSC 11) keeps tints that match it.
fn block_fills(view: &TerminalView) -> Fills {
    let cfg = view.settings.blocks;
    let rgb = |c: Rgb| hutan_settings::Rgb::new(c.0, c.1, c.2);
    let derived = derive_block_fills(
        rgb(view.theme.background()),
        rgb(view.theme.foreground()),
        rgb(cfg.accent),
        rgb(cfg.neutral),
    );
    let pick = |declared: Option<Tint>, derived: hutan_settings::Rgba| {
        tint_hsla(declared.unwrap_or_else(|| Tint::from(derived)))
    };
    Fills {
        separator: pick(cfg.separator, derived.separator),
        highlight: pick(cfg.highlight, derived.highlight),
        hover: pick(cfg.hover, derived.hover),
        folded: pick(cfg.folded, derived.folded),
    }
}

/// The cells of `row` that intersect columns `from..to`, as one row in `color`, with the
/// columns it covers. Bold, italic and underline stay; inverse, dim and hidden are replaced by
/// the one color.
fn selection_row(row: &Row, from: u16, to: u16, color: Rgb) -> Option<(u16, u16, Row)> {
    let keep = CellFlags::all() - CellFlags::INVERSE - CellFlags::DIM - CellFlags::HIDDEN;
    let mut runs: Vec<Run> = Vec::new();
    let (mut first, mut last) = (None, 0);
    for cell in row_cells(row)
        .into_iter()
        .filter(|c| c.end() > from && c.col < to)
    {
        first.get_or_insert(cell.col);
        last = cell.end();
        let flags = cell.run.flags & keep;
        match runs.last_mut() {
            Some(prev) if prev.flags == flags => prev.text.push_str(cell.text),
            _ => runs.push(Run {
                text: cell.text.to_owned(),
                fg: Color::Rgb(color.0, color.1, color.2),
                bg: Color::Default,
                flags,
                underline_color: None,
                link: None,
            }),
        }
    }
    Some((
        first?,
        last,
        Row {
            runs,
            wrapped: false,
        },
    ))
}

fn selection_text(
    row: &Row,
    from: u16,
    to: u16,
    color: Rgb,
    theme: &Theme,
    font: &FontSpec,
    text_system: &WindowTextSystem,
) -> Option<SelectionText> {
    let (from, to, recolored) = selection_row(row, from, to, color)?;
    Some(SelectionText {
        from,
        to,
        shaped: shape_row(&recolored, theme, font, RowHover::NONE, text_system),
    })
}

/// A run of rows drawn with one color: a block's gutter bar, or a highlight tint.
struct RowRun {
    from: u16,
    /// Exclusive.
    to: u16,
    color: Hsla,
}

/// The status chip on a block's prompt line.
struct ChipPaint {
    row: u16,
    col: u16,
    cols: u16,
    shaped: ShapedRow,
}

/// What the block overlay adds to the grid (see [`crate::blocks`]); empty when the overlay is
/// off. Everything is drawn over the grid: no cell moves.
#[derive(Default)]
struct OverlayPaint {
    bars: Vec<RowRun>,
    tints: Vec<RowRun>,
    separators: Vec<u16>,
    chips: Vec<ChipPaint>,
    separator: Hsla,
    gutter_width: Pixels,
}

struct CursorPaint {
    col: u16,
    row: u16,
    cols: u16,
    shape: CursorShape,
    color: Hsla,
    /// The character under a block cursor, drawn in the cursor foreground (default: the terminal background).
    glyph: Option<ShapedLine>,
}

struct MarkedPaint {
    col: u16,
    row: u16,
    cols: u16,
    shaped: ShapedRow,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    /// The padded box the grid is laid out in.
    content: Bounds<Pixels>,
    font: FontSpec,
    background: Hsla,
    selection: Hsla,
    rows: Vec<RowPaint>,
    overlay: OverlayPaint,
    cursor: Option<CursorPaint>,
    marked: Option<MarkedPaint>,
    cursor_style: CursorStyle,
}

impl Element for TerminalElement {
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
        let (family, font_size, line_height, padding) = {
            let settings = &self.view.read(cx).settings;
            (
                settings.font_family.clone(),
                settings.font_size,
                settings.line_height,
                settings.padding.max(0.0),
            )
        };
        let font = FontSpec::measure(&family, font_size, line_height, window.text_system());
        let content = padded(bounds, padding);
        let cols = ((content.size.width / font.cell_width).floor() as u16).max(1);
        let rows = ((content.size.height / font.cell_height).floor() as u16).max(1);
        let layout = Layout {
            bounds: content,
            cell: size(font.cell_width, font.cell_height),
            cols,
            rows,
            scale: window.scale_factor(),
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.view.update(cx, |view, cx| {
            view.update_layout(layout, cx);
            let mut prepaint = prepare(view, font, rows, hitbox, window);
            prepaint.content = content;
            prepaint
        })
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
        let interactive = self.view.read(cx).mode == ViewMode::Interactive;
        if interactive {
            let focus_handle = self.view.read(cx).focus_handle(cx);
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(state.content, self.view.clone()),
                cx,
            );
            window.set_cursor_style(state.cursor_style, &state.hitbox);
            register_mouse_listeners(&self.view, bounds, window);
        }

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_grid(bounds, state, window, cx);
        });
        self.view.update(cx, |view, _| view.painted());

        // Ack only what has been painted. Painting happens at most once per display tick, so a
        // flood is still paced by the display, and the ack goes out at once: waiting for the
        // next tick first (as this did) left the daemon without credit for a whole frame after
        // every paint, which doubled the latency of typing faster than ~25 keys/s (docs/perf.md).
        let shared = self.view.read(cx).shared().clone();
        if let Some(frame) = shared.update(cx, |shared, _| shared.take_ack()) {
            self.view.read(cx).send_ack(frame);
        }
    }
}

use gpui::Focusable as _;

/// Everything the paint pass needs, computed with the view borrowed.
fn prepare(
    view: &mut TerminalView,
    font: FontSpec,
    grid_rows: u16,
    hitbox: Hitbox,
    window: &mut Window,
) -> Prepaint {
    let focused = view.has_focus(window);
    let text_system = window.text_system().clone();
    let text_system = &*text_system;
    let hover_link = view.hover_link();
    let span = view.selection.and_then(|s| s.span(&view.model.get()));
    let fingerprint = view.theme.fingerprint();
    let model_ref = view.model.clone();
    let model = model_ref.get();
    let model_cols = model.cols();

    view.rows.begin_frame();
    let display = view.display();
    let mut rows = Vec::with_capacity(usize::from(grid_rows));
    for r in 0..grid_rows {
        let Some(shown) = display.get(usize::from(r)) else {
            rows.push(RowPaint {
                shaped: None,
                selected: None,
                selected_text: None,
                fill: None,
            });
            continue;
        };
        let line = shown.line();
        let selected = span.and_then(|s| s.columns_on(line, model_cols));
        if let DisplayRow::Summary(fold) = shown {
            let (background, foreground) = (view.theme.background(), view.theme.foreground());
            let text = format!("  {}", fold.summary());
            let styled = styled_row(
                &text,
                Color::Rgb(
                    foreground.mix(background, 0.4).0,
                    foreground.mix(background, 0.4).1,
                    foreground.mix(background, 0.4).2,
                ),
                CellFlags::ITALIC,
            );
            rows.push(RowPaint {
                shaped: Some(Rc::new(shape_row(
                    &styled,
                    &view.theme,
                    &font,
                    RowHover::NONE,
                    text_system,
                ))),
                selected,
                selected_text: None,
                fill: Some(block_fills(view).folded),
            });
            continue;
        }
        let row = model.row_at(line);
        let shaped = row.filter(|row| !row.runs.is_empty()).map(|row| {
            let hover = RowHover {
                link: hover_link,
                columns: view
                    .hover_detected
                    .as_ref()
                    .and_then(|detected| detected.columns_on(line)),
                strong: view.cmd_held,
            };
            let key = row_key(row, fingerprint, &font, hover);
            shape_cached(
                &mut view.rows,
                key,
                row,
                &view.theme,
                &font,
                hover,
                text_system,
            )
        });
        // A pack's `terminal.selection.foreground` recolors the selected text; without one
        // each cell keeps its own color.
        let selected_text = view
            .theme
            .selection_foreground()
            .zip(row.zip(selected))
            .and_then(|(color, (row, (from, to)))| {
                selection_text(row, from, to, color, &view.theme, &font, text_system)
            });
        rows.push(RowPaint {
            shaped,
            selected,
            selected_text,
            fill: None,
        });
    }
    let (overlay, report) = prepare_overlay(view, &display, grid_rows, &font, text_system);
    view.painted_overlay = report;

    let theme = &view.theme;
    let cursor = prepare_cursor(view, focused, &font, grid_rows, text_system);

    let cursor_row = view.cursor_row().unwrap_or(u16::MAX);
    let marked = view.marked_for_paint().and_then(|text| {
        let cursor = view.model.get().cursor();
        if cursor_row >= grid_rows {
            return None;
        }
        let row = styled_row(text, Color::Default, CellFlags::UNDERLINE);
        Some(MarkedPaint {
            col: cursor.col,
            row: cursor_row,
            cols: u16::try_from(text_columns(text)).unwrap_or(u16::MAX),
            shaped: shape_row(&row, theme, &font, RowHover::NONE, text_system),
        })
    });

    let cursor_style =
        if (view.hover_link().is_some() || view.hover_detected.is_some()) && view.cmd_held {
            CursorStyle::PointingHand
        } else if view.model.get().modes().mouse != hutan_protocol::render::MouseMode::None {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };

    Prepaint {
        hitbox,
        content: Bounds::default(),
        font,
        background: hsla(theme.background()),
        selection: hsla(theme.selection()),
        rows,
        overlay,
        cursor,
        marked,
        cursor_style,
    }
}

/// Opacity of a running block's bar: a slow triangle wave, or steady without animation.
fn running_alpha(phase: u8, animate: bool) -> f32 {
    if !animate {
        return 0.6;
    }
    let half = f32::from(ANIMATION_PHASES / 2);
    let t = f32::from(phase % ANIMATION_PHASES);
    let wave = if t <= half {
        t / half
    } else {
        (2.0 * half - t) / half
    };
    0.3 + 0.55 * wave
}

fn prepare_overlay(
    view: &TerminalView,
    display: &[DisplayRow],
    grid_rows: u16,
    font: &FontSpec,
    text_system: &WindowTextSystem,
) -> (OverlayPaint, OverlayReport) {
    let mut report = OverlayReport::default();
    if !view.blocks_visible() {
        return (OverlayPaint::default(), report);
    }
    let cfg = view.settings.blocks;
    let interactive = view.mode == ViewMode::Interactive;
    let model = view.model.get();
    let spans = view.block_spans();
    let rows = display.len().min(usize::from(grid_rows));
    let owner: Vec<Option<u64>> = display[..rows]
        .iter()
        .map(|shown| blocks::block_at(&spans, shown.line()))
        .collect();
    let fills = block_fills(view);
    let tone_color = |tone: Tone| match tone {
        Tone::Success => cfg.success,
        Tone::Error => cfg.error,
        Tone::Neutral => cfg.neutral,
    };

    // Runs of consecutive rows of one block.
    let mut runs: Vec<(u64, u16, u16)> = Vec::new();
    for (row, id) in owner.iter().enumerate() {
        let (Some(id), Ok(row)) = (id, u16::try_from(row)) else {
            continue;
        };
        match runs.last_mut() {
            Some((last, _, to)) if *last == *id && *to == row => *to = row + 1,
            _ => runs.push((*id, row, row + 1)),
        }
    }

    let hovered = if interactive {
        view.hover_block()
    } else {
        None
    };
    let mut overlay = OverlayPaint {
        gutter_width: px(cfg.gutter_width.max(0.0)),
        separator: fills.separator,
        ..OverlayPaint::default()
    };
    for (id, from, to) in &runs {
        let Some(block) = model.blocks().get(*id) else {
            continue;
        };
        if let Some(tone) = blocks::tone(block) {
            let alpha = match (block.state, tone) {
                (hutan_protocol::blocks::BlockState::Running, _) => {
                    running_alpha(view.animation_phase, cfg.animate && interactive)
                }
                (_, Tone::Neutral) => 0.5,
                _ => 0.9,
            };
            overlay.bars.push(RowRun {
                from: *from,
                to: *to,
                color: hsla(tone_color(tone)).opacity(alpha),
            });
            report.bars.push((*from, *to, tone));
        }
        if !interactive {
            continue;
        }
        if view.highlight == Some(*id) {
            report.highlighted.extend(*from..*to);
            overlay.tints.push(RowRun {
                from: *from,
                to: *to,
                color: fills.highlight,
            });
        } else if hovered == Some(*id) {
            overlay.tints.push(RowRun {
                from: *from,
                to: *to,
                color: fills.hover,
            });
        }
    }
    if !interactive {
        return (overlay, report);
    }
    report.summaries = display[..rows]
        .iter()
        .enumerate()
        .filter(|(_, shown)| matches!(shown, DisplayRow::Summary(_)))
        .filter_map(|(row, _)| u16::try_from(row).ok())
        .collect();

    let model_cols = model.cols();
    for span in &spans {
        let Some(row) = display[..rows]
            .iter()
            .position(|shown| *shown == DisplayRow::Line(span.first))
            .and_then(|row| u16::try_from(row).ok())
        else {
            continue;
        };
        let Some(block) = model.blocks().get(span.id) else {
            continue;
        };
        if row > 0 && block.prompt_line > 0 {
            overlay.separators.push(row);
            report.separators.push(row);
        }
        let (Some(text), Some(tone)) = (blocks::chip_text(block), blocks::tone(block)) else {
            continue;
        };
        let cols = u16::try_from(text_columns(&text)).unwrap_or(u16::MAX);
        let Some(col) = model_cols.checked_sub(cols.saturating_add(1)) else {
            continue;
        };
        // The chip never covers what the prompt line shows.
        let used = model.row_at(span.first).map_or(0, row_width);
        if col < 2 || used >= col {
            continue;
        }
        let color = tone_color(tone);
        let styled = styled_row(
            &text,
            Color::Rgb(color.0, color.1, color.2),
            CellFlags::empty(),
        );
        report.chips.push((row, text.clone()));
        overlay.chips.push(ChipPaint {
            row,
            col,
            cols,
            shaped: shape_row(&styled, &view.theme, font, RowHover::NONE, text_system),
        });
    }
    (overlay, report)
}

fn shape_cached(
    cache: &mut RowCache,
    key: u64,
    row: &hutan_protocol::render::Row,
    theme: &Theme,
    font: &FontSpec,
    hover: RowHover,
    text_system: &WindowTextSystem,
) -> Rc<ShapedRow> {
    cache.get_or_shape(key, || shape_row(row, theme, font, hover, text_system))
}

fn prepare_cursor(
    view: &TerminalView,
    focused: bool,
    font: &FontSpec,
    grid_rows: u16,
    text_system: &WindowTextSystem,
) -> Option<CursorPaint> {
    let cursor = view.model.get().cursor();
    let row = view.cursor_row().filter(|row| *row < grid_rows)?;
    if view.mode == ViewMode::Preview
        || view.scroll_offset != 0
        || !cursor.visible
        || cursor.col >= view.model.get().cols().max(1)
    {
        return None;
    }
    let (shape, blinks) = view.cursor_appearance();
    let shape = if focused {
        shape
    } else {
        CursorShape::HollowBlock
    };
    if focused && blinks && !view.blink_on {
        return None;
    }
    let model = view.model.get();
    let cell_row = model.row_at(model.viewport_line(cursor.row));
    let cell = cell_row.and_then(|row| {
        row_cells(row)
            .into_iter()
            .find(|c| c.col <= cursor.col && cursor.col < c.end())
    });
    let cols = cell.map_or(1, |c| u16::from(c.width));
    let glyph = match (shape, cell) {
        (CursorShape::Block, Some(cell))
            if !cell.text.trim().is_empty() && !cell.text.chars().all(char::is_control) =>
        {
            let flags = cell.run.flags & (CellFlags::BOLD | CellFlags::ITALIC | CellFlags::WIDE);
            let glyph = view.theme.cursor_foreground();
            let styled = styled_row(
                cell.text,
                Color::Rgb(glyph.0, glyph.1, glyph.2),
                flags - CellFlags::WIDE,
            );
            // `styled_row` re-derives the WIDE flag from the character itself.
            shape_row(&styled, &view.theme, font, RowHover::NONE, text_system).line
        }
        _ => None,
    };
    Some(CursorPaint {
        col: cursor.col,
        row,
        cols,
        shape,
        color: hsla(view.theme.cursor()),
        glyph,
    })
}

fn register_mouse_listeners(
    view: &Entity<TerminalView>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
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
                let inside = bounds.contains(&event.position);
                view.update(cx, |view, cx| view.mouse_move(event, inside, cx));
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
    window.on_key_event({
        let view = view.clone();
        move |event: &ModifiersChangedEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.modifiers_changed(event, cx));
            }
        }
    });
}

/// `bounds` inset by `padding` on every side (never negative).
fn padded(bounds: Bounds<Pixels>, padding: f32) -> Bounds<Pixels> {
    let pad = px(padding);
    let width = (bounds.size.width - pad * 2.0).max(px(0.0));
    let height = (bounds.size.height - pad * 2.0).max(px(0.0));
    Bounds::new(
        point(bounds.origin.x + pad, bounds.origin.y + pad),
        size(width, height),
    )
}

fn cell_bounds(
    origin: Point<Pixels>,
    font: &FontSpec,
    row: u16,
    col: u16,
    cols: u16,
) -> Bounds<Pixels> {
    Bounds::new(
        point(
            origin.x + font.cell_width * f32::from(col),
            origin.y + font.cell_height * f32::from(row),
        ),
        size(font.cell_width * f32::from(cols), font.cell_height),
    )
}

fn paint_grid(bounds: Bounds<Pixels>, state: &mut Prepaint, window: &mut Window, cx: &mut App) {
    let origin = state.content.origin;
    let font = state.font.clone();
    window.paint_quad(fill(bounds, state.background));

    // Backgrounds and selection first, so no row's fill covers a neighbour's glyph overhang.
    for (r, row) in state.rows.iter().enumerate() {
        let r = r as u16;
        if let Some(fill) = row.fill {
            window.paint_quad(fill_quad(origin, &font, r, state.content.size.width, fill));
        }
        if let Some(shaped) = &row.shaped {
            for bg in &shaped.backgrounds {
                window.paint_quad(fill(
                    cell_bounds(origin, &font, r, bg.col, bg.cols),
                    bg.color,
                ));
            }
        }
        if let Some((from, to)) = row.selected {
            window.paint_quad(fill(
                cell_bounds(origin, &font, r, from, to - from),
                state.selection,
            ));
        }
    }

    for tint in &state.overlay.tints {
        for r in tint.from..tint.to {
            window.paint_quad(fill_quad(
                origin,
                &font,
                r,
                state.content.size.width,
                tint.color,
            ));
        }
    }

    for (r, row) in state.rows.iter().enumerate() {
        let r = r as u16;
        let Some(shaped) = &row.shaped else { continue };
        let top_left = cell_bounds(origin, &font, r, 0, 0).origin;
        if let Some(line) = &shaped.line {
            let _ = line.paint(
                top_left,
                font.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        for decoration in &shaped.decorations {
            paint_decoration(decoration, origin, &font, r, window);
        }
        // The selection's own text color: cover the cells' original glyphs with the selection
        // fill, then draw the same cells in the pack's selection foreground.
        if let Some(text) = &row.selected_text {
            let span = cell_bounds(origin, &font, r, text.from, text.to - text.from);
            window.paint_quad(fill(span, state.selection));
            if let Some(line) = &text.shaped.line {
                let _ = line.paint(
                    span.origin,
                    font.cell_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            for decoration in &text.shaped.decorations {
                let shifted = Decoration {
                    col: decoration.col + text.from,
                    ..*decoration
                };
                paint_decoration(&shifted, origin, &font, r, window);
            }
        }
    }

    paint_overlay(
        &state.overlay,
        bounds,
        state.content,
        &font,
        &state.background,
        window,
        cx,
    );

    if let Some(marked) = &state.marked {
        let at = cell_bounds(origin, &font, marked.row, marked.col, marked.cols);
        window.paint_quad(fill(at, state.background));
        if let Some(line) = &marked.shaped.line {
            let _ = line.paint(
                at.origin,
                font.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        for decoration in &marked.shaped.decorations {
            let shifted = Decoration {
                col: decoration.col + marked.col,
                ..*decoration
            };
            paint_decoration(&shifted, origin, &font, marked.row, window);
        }
    }

    if let Some(cursor) = &state.cursor {
        paint_cursor(cursor, origin, &font, window, cx);
    }
}

/// A quad over the whole width of grid row `row`.
fn fill_quad(
    origin: Point<Pixels>,
    font: &FontSpec,
    row: u16,
    width: Pixels,
    color: Hsla,
) -> gpui::PaintQuad {
    fill(
        Bounds::new(
            point(origin.x, origin.y + font.cell_height * f32::from(row)),
            size(width, font.cell_height),
        ),
        color,
    )
}

/// Gutter bars, separators and chips, over the grid.
fn paint_overlay(
    overlay: &OverlayPaint,
    bounds: Bounds<Pixels>,
    content: Bounds<Pixels>,
    font: &FontSpec,
    background: &Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = content.origin;
    for chip in &overlay.chips {
        let at = cell_bounds(origin, font, chip.row, chip.col, chip.cols);
        window.paint_quad(fill(at, *background));
        if let Some(line) = &chip.shaped.line {
            let _ = line.paint(
                at.origin,
                font.cell_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
    for row in &overlay.separators {
        let y = origin.y + font.cell_height * f32::from(*row);
        window.paint_quad(fill(
            Bounds::new(point(origin.x, y), size(content.size.width, px(1.))),
            overlay.separator,
        ));
    }
    if overlay.gutter_width > px(0.) {
        for bar in &overlay.bars {
            let top = origin.y + font.cell_height * f32::from(bar.from) + px(1.);
            let bottom = origin.y + font.cell_height * f32::from(bar.to);
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.origin.x + px(1.), top),
                    size(overlay.gutter_width, (bottom - top).max(px(0.))),
                ),
                bar.color,
            ));
        }
    }
}

fn paint_cursor(
    cursor: &CursorPaint,
    origin: Point<Pixels>,
    font: &FontSpec,
    window: &mut Window,
    cx: &mut App,
) {
    let cell = cell_bounds(origin, font, cursor.row, cursor.col, cursor.cols);
    let thickness = px(2.);
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(cell, cursor.color));
            if let Some(glyph) = &cursor.glyph {
                let _ = glyph.paint(
                    cell.origin,
                    font.cell_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(cell, cursor.color, gpui::BorderStyle::Solid));
        }
        CursorShape::Bar => {
            window.paint_quad(fill(
                Bounds::new(cell.origin, size(thickness, cell.size.height)),
                cursor.color,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(cell.origin.x, cell.origin.y + cell.size.height - thickness),
                    size(cell.size.width, thickness),
                ),
                cursor.color,
            ));
        }
    }
}

fn paint_decoration(
    decoration: &Decoration,
    origin: Point<Pixels>,
    font: &FontSpec,
    row: u16,
    window: &mut Window,
) {
    let cell = cell_bounds(origin, font, row, decoration.col, decoration.cols);
    let one = px(1.);
    let baseline =
        cell.origin.y + (font.cell_height - font.ascent - font.descent) / 2. + font.ascent;
    let floor = cell.origin.y + font.cell_height - one;
    let y = (baseline + (font.descent * 0.5).max(one)).min(floor);
    let x = cell.origin.x;
    let width = cell.size.width;
    let line = |y: Pixels, x: Pixels, w: Pixels, window: &mut Window| {
        window.paint_quad(fill(
            Bounds::new(point(x, y), size(w, one)),
            decoration.color,
        ));
    };
    match decoration.kind {
        UnderlineKind::Single => line(y, x, width, window),
        UnderlineKind::Double => {
            let lower = (y + one).min(floor);
            line(lower, x, width, window);
            line(lower - one * 2., x, width, window);
        }
        UnderlineKind::Dotted => {
            let mut dx = px(0.);
            while dx < width {
                line(y, x + dx, one.min(width - dx), window);
                dx += one * 2.;
            }
        }
        UnderlineKind::Dashed => {
            let mut dx = px(0.);
            while dx < width {
                line(y, x + dx, (one * 3.).min(width - dx), window);
                dx += one * 5.;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::{run, wide_run};

    fn row_of(runs: Vec<Run>) -> Row {
        Row {
            runs,
            wrapped: false,
        }
    }

    #[test]
    fn the_selection_text_covers_only_the_selected_cells_in_one_color() {
        let bold = Run {
            flags: CellFlags::BOLD | CellFlags::INVERSE | CellFlags::DIM,
            fg: Color::Indexed(1),
            ..run("cd")
        };
        let row = row_of(vec![run("ab"), bold, run("ef")]);
        let (from, to, selected) = selection_row(&row, 1, 5, Rgb(1, 2, 3)).unwrap();
        // Columns 1..5 touch cells b, c, d, e: one column more at each end is not taken.
        assert_eq!((from, to), (1, 5));
        let text: Vec<_> = selected.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, ["b", "cd", "e"]);
        for r in &selected.runs {
            assert_eq!(r.fg, Color::Rgb(1, 2, 3));
            assert_eq!(r.bg, Color::Default);
            assert!(
                !r.flags
                    .intersects(CellFlags::INVERSE | CellFlags::DIM | CellFlags::HIDDEN)
            );
        }
        // Bold stays; the neighbours that were plain stay plain.
        assert_eq!(selected.runs[1].flags, CellFlags::BOLD);
        assert_eq!(selected.runs[0].flags, CellFlags::empty());
    }

    #[test]
    fn a_wide_character_on_the_edge_is_taken_whole_and_the_span_is_exact() {
        let row = row_of(vec![run("a"), wide_run("日"), run("z")]);
        // Column 2 is the right half of 日 (columns 1-2): the whole cell is recolored.
        let (from, to, selected) = selection_row(&row, 2, 3, Rgb(9, 9, 9)).unwrap();
        assert_eq!((from, to), (1, 3));
        assert_eq!(selected.runs.len(), 1);
        assert!(selected.runs[0].flags.contains(CellFlags::WIDE));
        // Nothing selected on this row's cells (past the end of the text): no recolor.
        assert!(selection_row(&row, 10, 12, Rgb(9, 9, 9)).is_none());
        assert!(selection_row(&row_of(vec![]), 0, 5, Rgb(9, 9, 9)).is_none());
    }
}
