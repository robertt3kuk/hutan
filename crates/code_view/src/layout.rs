//! Display columns of a line. Code is drawn on a cell grid, as the terminal is: every column is
//! one glyph `cell_width` wide (GPUI's `force_width`), so a column is a fixed x and hit testing,
//! selections and search matches are arithmetic. A tab runs to the next tab stop, a wide
//! character (CJK, most emoji) takes two cells and is followed by a space that stands in for its
//! second one, a control character is drawn as one replacement glyph, and a zero-width mark
//! rides on the glyph before it.

use std::ops::Range;

use unicode_width::UnicodeWidthChar as _;

/// Columns between tab stops.
pub const TAB_SIZE: usize = 4;

/// What a control character (other than a tab) is drawn as.
const CONTROL_GLYPH: char = '\u{FFFD}';

/// How many cells `c` takes when it starts at column `col`.
pub(crate) fn cells(c: char, col: usize) -> usize {
    match c {
        '\t' => TAB_SIZE - col % TAB_SIZE,
        c if c.is_control() => 1,
        c => c.width().unwrap_or(1),
    }
}

/// The columns of a whole line (the end of the last character).
pub fn line_columns(line: &str) -> usize {
    line_columns_at(line, 0)
}

/// The column where `text` ends when it starts at column `base` of its line (a tab runs to the
/// next stop of the line, not of `text`: the row of a wrapped line starts mid-line).
pub fn line_columns_at(text: &str, base: usize) -> usize {
    let mut col = base;
    for c in text.chars() {
        col += cells(c, col);
    }
    col
}

/// The column where the character at byte `byte` of `text` starts when `text` begins at column
/// `base` of its line (the end of the text for a byte at or past its end).
pub fn col_of_byte_at(text: &str, base: usize, byte: usize) -> usize {
    let mut col = base;
    for (at, c) in text.char_indices() {
        if at >= byte {
            return col;
        }
        col += cells(c, col);
    }
    col
}

/// The byte of `line` a click at column position `x_cols` (fractional, in cells) lands on: the
/// boundary nearest to it, so a click on the right half of a character goes after it.
pub fn byte_of_col(line: &str, x_cols: f32) -> usize {
    byte_of_col_at(line, 0, x_cols)
}

/// [`byte_of_col`] for text that starts at column `base` of its line; `x_cols` is a column of
/// the line.
pub fn byte_of_col_at(text: &str, base: usize, x_cols: f32) -> usize {
    let mut col = base;
    for (at, c) in text.char_indices() {
        let width = cells(c, col);
        if width > 0 && x_cols < col as f32 + width as f32 / 2.0 {
            return at;
        }
        col += width;
    }
    text.len()
}

/// The byte of `line` where a caret that wants column `goal` goes on it: the last character
/// boundary at or before the column (the end of a shorter line), never inside a wide character
/// or a tab.
pub fn byte_at_col(line: &str, goal: usize) -> usize {
    byte_at_col_from(line, 0, goal)
}

/// [`byte_at_col`] for text that starts at column `base` of its line; `goal` is a column of the
/// line.
pub fn byte_at_col_from(text: &str, base: usize, goal: usize) -> usize {
    let mut col = base;
    for (at, c) in text.char_indices() {
        let width = cells(c, col);
        if width > 0 && col + width > goal {
            return at;
        }
        col += width;
    }
    text.len()
}

/// The part of a line drawn for columns `from..to`: the text to shape (one glyph per cell) and
/// where each source character went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    /// One glyph per cell from column `start_col`.
    pub text: String,
    /// The first column `text` shows.
    pub start_col: usize,
    /// Cells `text` covers.
    pub cols: usize,
    /// For every source character shown, `(source byte, text byte)`, in order, then one entry
    /// for the end of what is shown.
    map: Vec<(usize, usize)>,
}

impl DisplayLine {
    /// Columns `from..to` of `line` (a character cut by either edge is drawn as blanks for the
    /// cells inside).
    pub fn build(line: &str, from: usize, to: usize) -> DisplayLine {
        Self::build_at(line, 0, from, to)
    }

    /// Columns `from..to` of a line, given `text`, the part of it that starts at column `base`
    /// (the row of a wrapped line).
    pub fn build_at(line: &str, base: usize, from: usize, to: usize) -> DisplayLine {
        let mut text = String::new();
        let mut map = Vec::new();
        let mut col = base;
        let mut cols = 0usize;
        let mut end_byte = line.len();
        for (at, c) in line.char_indices() {
            if col >= to {
                end_byte = at;
                break;
            }
            let width = cells(c, col);
            let next = col + width;
            if width == 0 {
                // A combining mark: it rides on the glyph before it.
                if col > from && col <= to {
                    map.push((at, text.len()));
                    text.push(c);
                }
                continue;
            }
            if next <= from {
                col = next;
                continue;
            }
            map.push((at, text.len()));
            let shown = next.min(to) - col.max(from);
            cols += shown;
            let whole = col >= from && next <= to;
            match c {
                '\t' => text.extend(std::iter::repeat_n(' ', shown)),
                _ if !whole => text.extend(std::iter::repeat_n(' ', shown)),
                c if c.is_control() => text.push(CONTROL_GLYPH),
                c => {
                    text.push(c);
                    // The second cell of a wide character.
                    text.extend(std::iter::repeat_n(' ', width - 1));
                }
            }
            col = next;
        }
        map.push((end_byte, text.len()));
        DisplayLine {
            text,
            start_col: from,
            cols,
            map,
        }
    }

    /// The bytes of `text` that show the source bytes `range` (clamped to what is shown).
    pub fn text_range(&self, range: Range<usize>) -> Range<usize> {
        let at = |byte: usize| -> usize {
            match self.map.binary_search_by(|(source, _)| source.cmp(&byte)) {
                Ok(i) => self.map[i].1,
                // Inside a character, or before / after what is shown: the next boundary.
                Err(i) => self.map.get(i).map_or(self.text.len(), |e| e.1),
            }
        };
        let start = at(range.start);
        let end = at(range.end).max(start);
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_run_to_the_next_stop() {
        assert_eq!(line_columns("\tx"), 5);
        assert_eq!(line_columns("ab\tx"), 5);
        assert_eq!(col_of_byte_at("ab\tx", 0, 3), 4);
        // A row that starts at column 2 of its line reaches the same stop.
        assert_eq!(col_of_byte_at("\tx", 2, 1), 4);
        assert_eq!(line_columns_at("\tx", 2), 5);
        let shown = DisplayLine::build("ab\tx", 0, 80);
        assert_eq!(shown.text, "ab  x");
        assert_eq!(shown.text_range(3..4), 4..5);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let line = "a日b";
        assert_eq!(line_columns(line), 4);
        assert_eq!(col_of_byte_at(line, 0, 4), 3);
        let shown = DisplayLine::build(line, 0, 80);
        assert_eq!(shown.text, "a日 b");
        assert_eq!(shown.cols, 4);
        // The source bytes of 日 are its glyph and its stand-in cell.
        assert_eq!(&shown.text[shown.text_range(1..4)], "日 ");
        // A click on the right half of 日 lands after it.
        assert_eq!(byte_of_col(line, 2.6), 4);
        assert_eq!(byte_of_col(line, 1.2), 1);
    }

    #[test]
    fn a_window_cuts_characters_into_blanks() {
        let line = "a日b\tc";
        // Columns 2..5: the second half of 日, b, the tab's first cell.
        let shown = DisplayLine::build(line, 2, 5);
        assert_eq!(shown.text, " b ");
        assert_eq!(shown.start_col, 2);
        assert_eq!(&shown.text[shown.text_range(4..5)], "b");
    }

    #[test]
    fn controls_are_one_visible_cell_and_marks_ride_on_their_base() {
        assert_eq!(DisplayLine::build("a\u{1}b", 0, 10).text, "a\u{FFFD}b");
        let shown = DisplayLine::build("e\u{301}x", 0, 10);
        assert_eq!(shown.text, "e\u{301}x");
        assert_eq!(shown.cols, 2, "the mark is a char of its own but no column");
        assert_eq!(line_columns("e\u{301}x"), 2);
    }

    #[test]
    fn ranges_outside_the_window_clamp() {
        let line = "0123456789";
        let shown = DisplayLine::build(line, 3, 6);
        assert_eq!(shown.text, "345");
        assert_eq!(shown.text_range(0..10), 0..3);
        assert_eq!(shown.text_range(0..2), 0..0);
        assert_eq!(shown.text_range(8..9), 3..3);
    }
}
