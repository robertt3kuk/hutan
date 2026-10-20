//! The text a parse reads and the edits that describe how it changed, without tying the crate to
//! one text type: a `String` works, and `hutan_buffer::Text` does with the `buffer` feature.

use std::ops::Range;
use std::sync::Arc;

use crate::error::SyntaxError;

/// A position as tree-sitter counts it: zero-based row and the column in **bytes** from the start
/// of the row. (The buffer's `Point` counts `char`s; convert with the byte offsets.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    /// Zero-based row.
    pub row: usize,
    /// Zero-based column in bytes.
    pub column: usize,
}

impl Point {
    /// The position after `text` was inserted at `self`.
    pub fn after(self, text: &str) -> Point {
        let newlines = text.bytes().filter(|b| *b == b'\n').count();
        match text.rfind('\n') {
            None => Point {
                row: self.row,
                column: self.column + text.len(),
            },
            Some(last) => Point {
                row: self.row + newlines,
                column: text.len() - last - 1,
            },
        }
    }
}

/// Read access to an immutable text snapshot, from any thread.
///
/// A parse is handed one snapshot per version, so implementations are usually a rope clone (`O(1)`
/// for `hutan_buffer::Text`) or an `Arc<str>`; the text never changes under the parse.
///
/// Implementations must not panic: they are called from inside tree-sitter's C parser, and a panic
/// cannot unwind through it (the process aborts).
pub trait TextProvider: Send + Sync {
    /// Length in bytes.
    fn len(&self) -> usize;

    /// A slice of the text that starts exactly at byte `offset`, of any length ≥ 1 up to the end
    /// of the implementation's current chunk; empty only when `offset >= len()`. `offset` is
    /// usually a `char` boundary but not guaranteed to be: slice the chunk's bytes, do not
    /// re-validate UTF-8.
    fn chunk(&self, offset: usize) -> &[u8];

    /// The row and byte column of `offset` (clamped to the length). The default scans from the
    /// start (`O(n)`); a rope should override it.
    fn point_at(&self, offset: usize) -> Point {
        let end = offset.min(self.len());
        let mut at = 0;
        let mut row = 0;
        let mut line_start = 0;
        while at < end {
            let chunk = self.chunk(at);
            let take = chunk.len().min(end - at);
            if take == 0 {
                break;
            }
            for (i, byte) in chunk[..take].iter().enumerate() {
                if *byte == b'\n' {
                    row += 1;
                    line_start = at + i + 1;
                }
            }
            at += take;
        }
        Point {
            row,
            column: end - line_start,
        }
    }

    /// The byte offset where `row` starts (the length when the text has fewer rows). The default
    /// scans (`O(n)`); a rope should override it.
    fn line_start(&self, row: usize) -> usize {
        if row == 0 {
            return 0;
        }
        let len = self.len();
        let mut at = 0;
        let mut seen = 0;
        while at < len {
            let chunk = self.chunk(at);
            if chunk.is_empty() {
                break;
            }
            for (i, byte) in chunk.iter().enumerate() {
                if *byte == b'\n' {
                    seen += 1;
                    if seen == row {
                        return at + i + 1;
                    }
                }
            }
            at += chunk.len();
        }
        len
    }

    /// Whether the text is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TextProvider for str {
    fn len(&self) -> usize {
        str::len(self)
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        self.as_bytes().get(offset..).unwrap_or_default()
    }
}

impl TextProvider for String {
    fn len(&self) -> usize {
        String::len(self)
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        self.as_bytes().get(offset..).unwrap_or_default()
    }
}

impl TextProvider for &'static str {
    fn len(&self) -> usize {
        str::len(self)
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        self.as_bytes().get(offset..).unwrap_or_default()
    }
}

impl<T: TextProvider + ?Sized> TextProvider for Arc<T> {
    fn len(&self) -> usize {
        (**self).len()
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        (**self).chunk(offset)
    }

    fn point_at(&self, offset: usize) -> Point {
        (**self).point_at(offset)
    }

    fn line_start(&self, row: usize) -> usize {
        (**self).line_start(row)
    }
}

/// One edit of the text, in the coordinates of the text just before it (edits of a transaction
/// are applied one after the other; see [`TextEdit::new`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextEdit {
    /// Where the replaced range starts (in the old and the new text).
    pub start: usize,
    /// Where the replaced range ended in the old text.
    pub old_end: usize,
    /// Where the replacement ends in the new text.
    pub new_end: usize,
    /// `start` as a row/column.
    pub start_point: Point,
    /// `old_end` as a row/column in the old text.
    pub old_end_point: Point,
    /// `new_end` as a row/column in the new text.
    pub new_end_point: Point,
}

impl TextEdit {
    /// Describes replacing `range` of `old_text` (the text before the edit) with `inserted`.
    /// Reads the old text only for the two row/column positions.
    pub fn new(
        old_text: &(impl TextProvider + ?Sized),
        range: Range<usize>,
        inserted: &str,
    ) -> Result<TextEdit, SyntaxError> {
        if range.start > range.end {
            return Err(SyntaxError::InvalidEdit("range start is after its end"));
        }
        if range.end > old_text.len() {
            return Err(SyntaxError::InvalidEdit(
                "range is past the end of the text",
            ));
        }
        let start_point = old_text.point_at(range.start);
        let old_end_point = if range.is_empty() {
            start_point
        } else {
            old_text.point_at(range.end)
        };
        Ok(TextEdit {
            start: range.start,
            old_end: range.end,
            new_end: range.start + inserted.len(),
            start_point,
            old_end_point,
            new_end_point: start_point.after(inserted),
        })
    }

    /// Change of the text length.
    pub fn delta(&self) -> isize {
        // Offsets are far below `isize::MAX` (they index in-memory text).
        self.new_end as isize - self.old_end as isize
    }

    pub(crate) fn to_input_edit(self) -> tree_sitter::InputEdit {
        tree_sitter::InputEdit {
            start_byte: self.start,
            old_end_byte: self.old_end,
            new_end_byte: self.new_end,
            start_position: ts_point(self.start_point),
            old_end_position: ts_point(self.old_end_point),
            new_end_position: ts_point(self.new_end_point),
        }
    }
}

pub(crate) fn ts_point(point: Point) -> tree_sitter::Point {
    tree_sitter::Point {
        row: point.row,
        column: point.column,
    }
}

pub(crate) fn from_ts_point(point: tree_sitter::Point) -> Point {
    Point {
        row: point.row,
        column: point.column,
    }
}

/// Adapts a [`TextProvider`] to tree-sitter's predicate evaluation (`#eq?`, `#match?`): the
/// chunks of one node's text.
pub(crate) struct NodeText<'a>(pub &'a dyn TextProvider);

pub(crate) struct NodeChunks<'a> {
    text: &'a dyn TextProvider,
    at: usize,
    end: usize,
}

impl<'a> Iterator for NodeChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.at >= self.end {
            return None;
        }
        let chunk = self.text.chunk(self.at);
        let take = chunk.len().min(self.end - self.at);
        if take == 0 {
            return None;
        }
        self.at += take;
        Some(&chunk[..take])
    }
}

impl<'a> tree_sitter::TextProvider<&'a [u8]> for NodeText<'a> {
    type I = NodeChunks<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> NodeChunks<'a> {
        let range = node.byte_range();
        NodeChunks {
            text: self.0,
            at: range.start,
            end: range.end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_points_and_lines() {
        let text = "ab\ncde\n\nf";
        assert_eq!(text.point_at(0), Point { row: 0, column: 0 });
        assert_eq!(text.point_at(2), Point { row: 0, column: 2 });
        assert_eq!(text.point_at(3), Point { row: 1, column: 0 });
        assert_eq!(text.point_at(7), Point { row: 2, column: 0 });
        assert_eq!(text.point_at(100), Point { row: 3, column: 1 });
        assert_eq!(text.line_start(0), 0);
        assert_eq!(text.line_start(1), 3);
        assert_eq!(text.line_start(3), 8);
        assert_eq!(text.line_start(9), text.len());
    }

    #[test]
    fn edit_points() {
        let text = "ab\ncde\nf";
        let edit = TextEdit::new(text, 1..5, "X\nYZ\n").unwrap();
        assert_eq!(edit.start_point, Point { row: 0, column: 1 });
        assert_eq!(edit.old_end_point, Point { row: 1, column: 2 });
        assert_eq!(edit.new_end, 6);
        assert_eq!(edit.new_end_point, Point { row: 2, column: 0 });
        assert_eq!(edit.delta(), 1);
        #[allow(clippy::reversed_empty_ranges)] // the point of the test
        let backwards = 3..2;
        assert!(TextEdit::new(text, backwards, "").is_err());
        assert!(TextEdit::new(text, 0..99, "").is_err());
    }

    #[test]
    fn point_after_text() {
        let p = Point { row: 4, column: 3 };
        assert_eq!(p.after("abc"), Point { row: 4, column: 6 });
        assert_eq!(p.after("a\nbc"), Point { row: 5, column: 2 });
        assert_eq!(p.after("\n"), Point { row: 5, column: 0 });
    }
}
