//! Text shaping on the cell grid and the per-row cache.
//!
//! A row is shaped as one line whose glyphs GPUI snaps to `column × cell width` (`force_width`).
//! GPUI places the n-th base glyph at n cells, so every cell must produce exactly one glyph:
//! a wide character is followed by a space that stands in for its second column, and
//! combining marks (zero advance) ride on their base glyph. Font fallback therefore cannot shift
//! later columns, and ligatures are disabled so two cells never merge into one glyph.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gpui::{
    Font, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, ShapedLine, SharedString,
    StrikethroughStyle, TextRun, UnderlineStyle, WindowTextSystem, px, rgb,
};
use hutan_protocol::render::{CellFlags, Color, Row, Run};

use crate::model::{Cell, row_cells};
use crate::theme::{Rgb, Theme};

pub(crate) fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_hex()).into()
}

/// Font and cell geometry text is shaped with.
#[derive(Clone, Debug)]
pub(crate) struct FontSpec {
    pub family: SharedString,
    pub size: Pixels,
    pub cell_width: Pixels,
    pub cell_height: Pixels,
    pub ascent: Pixels,
    pub descent: Pixels,
}

impl FontSpec {
    /// Metrics for `family` at `font_size`: the cell is as wide as `M` and `line_height` times
    /// the font size tall.
    pub fn measure(
        family: &str,
        font_size: f32,
        line_height: f32,
        text_system: &WindowTextSystem,
    ) -> Self {
        let size = px(font_size.max(1.0));
        let mut spec = Self {
            family: SharedString::from(family.to_owned()),
            size,
            cell_width: size * 0.6,
            cell_height: size * line_height.max(0.5),
            ascent: size * 0.8,
            descent: size * 0.2,
        };
        let font_id = text_system.resolve_font(&spec.font(false, false));
        if let Ok(advance) = text_system.advance(font_id, size, 'M')
            && advance.width > px(0.)
        {
            spec.cell_width = advance.width;
        }
        spec.ascent = text_system.ascent(font_id, size);
        spec.descent = text_system.descent(font_id, size).abs();
        spec
    }

    pub fn font(&self, bold: bool, italic: bool) -> Font {
        Font {
            family: self.family.clone(),
            features: FontFeatures(std::sync::Arc::new(vec![
                ("calt".to_owned(), 0),
                ("liga".to_owned(), 0),
            ])),
            fallbacks: None,
            weight: if bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        }
    }

    fn hash_into(&self, state: &mut impl Hasher) {
        self.family.hash(state);
        f32::from(self.size).to_bits().hash(state);
        f32::from(self.cell_width).to_bits().hash(state);
        f32::from(self.cell_height).to_bits().hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnderlineKind {
    Single,
    Double,
    Dotted,
    Dashed,
}

/// A decoration painted as quads over `cols` columns starting at `col`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Decoration {
    pub col: u16,
    pub cols: u16,
    pub kind: UnderlineKind,
    pub color: Hsla,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Background {
    pub col: u16,
    pub cols: u16,
    pub color: Hsla,
}

#[derive(Debug, Default)]
pub(crate) struct ShapedRow {
    pub line: Option<ShapedLine>,
    pub backgrounds: Vec<Background>,
    pub decorations: Vec<Decoration>,
}

fn underline_kind(flags: CellFlags) -> Option<UnderlineKind> {
    if flags.contains(CellFlags::DOUBLE_UNDERLINE) {
        Some(UnderlineKind::Double)
    } else if flags.contains(CellFlags::DOTTED_UNDERLINE) {
        Some(UnderlineKind::Dotted)
    } else if flags.contains(CellFlags::DASHED_UNDERLINE) {
        Some(UnderlineKind::Dashed)
    } else if flags.contains(CellFlags::UNDERLINE) {
        Some(UnderlineKind::Single)
    } else {
        None
    }
}

fn push_merged<T>(list: &mut Vec<T>, item: T, merge: impl FnOnce(&mut T, &T) -> bool) {
    if let Some(last) = list.last_mut()
        && merge(last, &item)
    {
        return;
    }
    list.push(item);
}

/// What the pointer underlines on one row while ⌘ is held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RowHover {
    /// The hovered OSC 8 hyperlink: its runs are underlined.
    pub link: Option<u32>,
    /// The hovered link found in the text: its columns on this row, `from..to`.
    pub columns: Option<(u16, u16)>,
    /// ⌘ is held: the link is underlined solid, it opens on a click. Otherwise (a plain hover)
    /// the underline is dotted, the quiet hint that the text is a link.
    pub strong: bool,
}

impl RowHover {
    pub const NONE: Self = Self {
        link: None,
        columns: None,
        strong: false,
    };

    /// How a run's cells are underlined by the hover, if they are.
    fn underline(&self, run: &Run, cell_from: u16, cell_to: u16) -> Option<UnderlineKind> {
        if self.link.is_some() && run.link == self.link {
            return Some(UnderlineKind::Single);
        }
        self.columns
            .is_some_and(|(from, to)| from <= cell_from && cell_to <= to)
            .then_some(if self.strong {
                UnderlineKind::Single
            } else {
                UnderlineKind::Dotted
            })
    }
}

/// Shapes `row` for painting at column 0. `hover` underlines the hovered link.
pub(crate) fn shape_row(
    row: &Row,
    theme: &Theme,
    font: &FontSpec,
    hover: RowHover,
    text_system: &WindowTextSystem,
) -> ShapedRow {
    let cells = row_cells(row);
    let mut out = ShapedRow::default();
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<*const Run> = None;

    for cell in &cells {
        let run = cell.run;
        let colors = theme.resolve_run(run.fg, run.bg, run.flags);
        let fg = hsla(colors.fg);
        let cols = u16::from(cell.width);

        if colors.paint_bg {
            push_merged(
                &mut out.backgrounds,
                Background {
                    col: cell.col,
                    cols,
                    color: hsla(colors.bg),
                },
                |last, new| {
                    let adjacent = last.col + last.cols == new.col && last.color == new.color;
                    if adjacent {
                        last.cols += new.cols;
                    }
                    adjacent
                },
            );
        }

        let start = text.len();
        push_cell_text(&mut text, cell, run.flags.contains(CellFlags::HIDDEN));
        let len = text.len() - start;

        let ul_color = run
            .underline_color
            .map_or(fg, |c| hsla(theme.resolve(c, colors.fg)));
        let mut kind = underline_kind(run.flags);
        if kind.is_none() {
            kind = hover.underline(run, cell.col, cell.end());
        }
        if let Some(kind) = kind {
            push_merged(
                &mut out.decorations,
                Decoration {
                    col: cell.col,
                    cols,
                    kind,
                    color: ul_color,
                },
                |last, new| {
                    let adjacent = last.col + last.cols == new.col
                        && last.kind == new.kind
                        && last.color == new.color;
                    if adjacent {
                        last.cols += new.cols;
                    }
                    adjacent
                },
            );
        }

        if current == Some(std::ptr::from_ref(run)) {
            if let Some(last) = runs.last_mut() {
                last.len += len;
            }
        } else {
            current = Some(std::ptr::from_ref(run));
            runs.push(TextRun {
                len,
                font: font.font(
                    run.flags.contains(CellFlags::BOLD),
                    run.flags.contains(CellFlags::ITALIC),
                ),
                color: fg,
                background_color: None,
                underline: run
                    .flags
                    .contains(CellFlags::UNDERCURL)
                    .then_some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(ul_color),
                        wavy: true,
                    }),
                strikethrough: run.flags.contains(CellFlags::STRIKETHROUGH).then_some(
                    StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(fg),
                    },
                ),
            });
        }
    }

    if !text.is_empty() {
        out.line = Some(text_system.shape_line(
            SharedString::from(text),
            font.size,
            &runs,
            Some(font.cell_width),
        ));
    }
    out
}

/// One glyph per cell: wide characters get a trailing space, control characters and hidden
/// text become blanks.
fn push_cell_text(text: &mut String, cell: &Cell<'_>, hidden: bool) {
    let blank = hidden || cell.text.chars().next().is_none_or(char::is_control);
    if blank {
        text.push(' ');
    } else {
        text.push_str(cell.text);
    }
    if cell.width == 2 {
        text.push(' ');
    }
}

fn hash_color(color: Color, state: &mut impl Hasher) {
    match color {
        Color::Default => 0u8.hash(state),
        Color::Indexed(i) => (1u8, i).hash(state),
        Color::Rgb(r, g, b) => (2u8, r, g, b).hash(state),
    }
}

/// Content key of a row: everything that influences its shaped form.
pub(crate) fn row_key(row: &Row, style_key: u64, font: &FontSpec, hover: RowHover) -> u64 {
    let mut h = DefaultHasher::new();
    style_key.hash(&mut h);
    font.hash_into(&mut h);
    for run in &row.runs {
        run.text.hash(&mut h);
        hash_color(run.fg, &mut h);
        hash_color(run.bg, &mut h);
        run.flags.hash(&mut h);
        run.underline_color.is_some().hash(&mut h);
        if let Some(c) = run.underline_color {
            hash_color(c, &mut h);
        }
        (hover.link.is_some() && run.link == hover.link).hash(&mut h);
    }
    hover.columns.hash(&mut h);
    hover.strong.hash(&mut h);
    h.finish()
}

/// Shaped rows by content, so scrolling and unchanged rows cost a hash, not a shaping pass.
#[derive(Default)]
pub(crate) struct RowCache {
    entries: HashMap<u64, (Rc<ShapedRow>, u64)>,
    generation: u64,
}

impl std::fmt::Debug for RowCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowCache")
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// Entries kept once the cache outgrows this; the current frame's rows always survive.
const CACHE_SOFT_LIMIT: usize = 1024;

impl RowCache {
    pub fn begin_frame(&mut self) {
        self.generation += 1;
        if self.entries.len() > CACHE_SOFT_LIMIT {
            let keep_from = self.generation.saturating_sub(1);
            self.entries.retain(|_, (_, used)| *used >= keep_from);
        }
    }

    pub fn get_or_shape(&mut self, key: u64, shape: impl FnOnce() -> ShapedRow) -> Rc<ShapedRow> {
        let generation = self.generation;
        let entry = self
            .entries
            .entry(key)
            .and_modify(|(_, used)| *used = generation)
            .or_insert_with(|| (Rc::new(shape()), generation));
        entry.0.clone()
    }
}

/// A row made of `text` with one style, split into runs of equal character width. Used to draw
/// IME marked text and the character under a block cursor with the regular row shaper.
pub(crate) fn styled_row(text: &str, fg: Color, flags: CellFlags) -> Row {
    use unicode_width::UnicodeWidthChar;
    let mut runs: Vec<Run> = Vec::new();
    for c in text.chars() {
        let wide = c.width() == Some(2);
        let zero = c.width() == Some(0);
        let run_flags = if wide {
            flags | CellFlags::WIDE
        } else {
            flags - CellFlags::WIDE
        };
        match runs.last_mut() {
            Some(last) if zero || last.flags == run_flags => last.text.push(c),
            _ => runs.push(Run {
                text: c.to_string(),
                fg,
                bg: Color::Default,
                flags: run_flags,
                underline_color: None,
                link: None,
            }),
        }
    }
    Row {
        runs,
        wrapped: false,
    }
}

/// Columns `text` occupies on the grid.
pub(crate) fn text_columns(text: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    text.chars().map(|c| c.width().unwrap_or(1)).sum()
}

/// Shaping against the real macOS text system: font fallback must not move later columns.
#[cfg(all(test, target_os = "macos"))]
mod real_font_tests {
    use std::sync::Arc;

    use gpui::TextSystem;

    use super::*;
    use crate::model::fixtures::{run, wide_run};
    use crate::theme::TerminalPalette;

    fn text_system() -> WindowTextSystem {
        WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(
            gpui_apple::AppleTextSystem::new(),
        ))))
    }

    /// Byte offset in the shaped text of every cell, as `shape_row` builds it.
    fn cell_offsets(row: &Row) -> Vec<(u16, usize)> {
        let mut text = String::new();
        row_cells(row)
            .iter()
            .map(|cell| {
                let at = text.len();
                push_cell_text(&mut text, cell, false);
                (cell.col, at)
            })
            .collect()
    }

    #[test]
    fn fallback_fonts_keep_glyphs_on_the_cell_grid() {
        let ts = text_system();
        let font = FontSpec::measure("Menlo", 13.0, 1.2, &ts);
        let cell = f32::from(font.cell_width);
        assert!(cell > 6.0 && cell < 10.0, "Menlo 13 cell width {cell}");
        let row = Row {
            runs: vec![
                run("ab"),
                wide_run("日本"),
                run("cЖ→✓d"),
                wide_run("🙂"),
                run("e\u{301}x\u{2764}\u{fe0f}y"),
                wide_run("語"),
                run("end"),
            ],
            wrapped: false,
        };
        let theme = Theme::new(TerminalPalette::default());
        let shaped = shape_row(&row, &theme, &font, RowHover::NONE, &ts);
        let line = shaped.line.expect("line");
        let offsets = cell_offsets(&row);
        assert!(offsets.len() > 15);
        for (col, byte) in offsets {
            let x = f32::from(line.x_for_index(byte));
            let want = f32::from(col) * cell;
            assert!(
                (x - want).abs() < 0.75,
                "column {col}: glyph at {x}px, grid says {want}px"
            );
        }
    }
}
