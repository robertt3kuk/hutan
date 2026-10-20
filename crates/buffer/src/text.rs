use std::fmt;
use std::ops::Range;

use crop::Rope;

use crate::edit::Edit;
use crate::error::BufferError;

/// A position as (line, column) where the column counts Unicode scalar values (`char`s) from the
/// start of the line. Both are zero-based.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    /// Zero-based line.
    pub line: usize,
    /// Zero-based column in `char`s.
    pub column: usize,
}

/// A position as LSP and GPUI's text input express it: zero-based line, and the column in UTF-16
/// code units (LSP calls it `character`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utf16Point {
    /// Zero-based line.
    pub line: usize,
    /// Zero-based column in UTF-16 code units.
    pub character: usize,
}

/// An immutable-by-API view of the buffer's text: a rope with byte offsets.
///
/// Cloning is `O(1)` (the rope shares its tree), so a clone is a cheap snapshot for background
/// work such as parsing or LSP synchronization; it never changes after it is taken. Only
/// [`Buffer`](crate::Buffer) mutates its own text.
///
/// Every offset is a byte offset unless the name says otherwise (`*_utf16`, `*_char`, [`Point`]
/// columns). Offsets must lie on `char` boundaries; functions that take caller input return
/// [`BufferError`] instead of panicking.
///
/// ## Lines
/// A line ends at `\n`, or at `\r\n` (a lone `\r` is an ordinary character). The text always has at
/// least one line, and a trailing line break starts a final empty line, as in every editor:
/// `"a\n"` has two lines. [`line_range`](Self::line_range) includes the terminator,
/// [`line_content_range`](Self::line_content_range) does not.
#[derive(Clone, Default)]
pub struct Text {
    rope: Rope,
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Text")
            .field("bytes", &self.len())
            .field("lines", &self.line_count())
            .finish()
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.rope.chunks().try_for_each(|chunk| f.write_str(chunk))
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self {
            rope: Rope::from(s),
        }
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.rope == *other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.rope == **other
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.rope == other.rope
    }
}

impl Eq for Text {}

impl Text {
    /// The empty text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.rope.byte_len()
    }

    /// Whether the text has no bytes.
    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    /// Length in UTF-16 code units.
    pub fn utf16_len(&self) -> usize {
        self.rope.utf16_len()
    }

    /// Number of lines (at least 1; a trailing line break adds an empty last line).
    pub fn line_count(&self) -> usize {
        self.rope.line_of_byte(self.rope.byte_len()) + 1
    }

    /// The text's chunks in order: borrowed `&str`s of at most ~1 KiB, never splitting a character.
    pub fn chunks(&self) -> impl Iterator<Item = &str> + '_ {
        self.rope.chunks()
    }

    /// The chunks of a byte range.
    pub fn chunks_in(
        &self,
        range: Range<usize>,
    ) -> Result<impl Iterator<Item = &str> + '_, BufferError> {
        self.check_range(&range)?;
        Ok(self.rope.byte_slice(range).chunks())
    }

    /// Copies a byte range into a `String`.
    pub fn slice(&self, range: Range<usize>) -> Result<String, BufferError> {
        let mut out = String::with_capacity(range.len());
        for chunk in self.chunks_in(range)? {
            out.push_str(chunk);
        }
        Ok(out)
    }

    /// The `char` that starts at `offset`, or `None` at the end or off a boundary.
    pub fn char_at(&self, offset: usize) -> Option<char> {
        if offset >= self.len() || !self.rope.is_char_boundary(offset) {
            return None;
        }
        self.rope.byte_slice(offset..).chars().next()
    }

    /// The `char` that ends at `offset`, or `None` at the start or off a boundary.
    pub fn char_before(&self, offset: usize) -> Option<char> {
        if offset == 0 || offset > self.len() || !self.rope.is_char_boundary(offset) {
            return None;
        }
        self.rope.byte_slice(..offset).chars().next_back()
    }

    /// Whether `offset` is within the text and on a `char` boundary.
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        offset <= self.len() && self.rope.is_char_boundary(offset)
    }

    /// The largest boundary offset that is `<= offset` (and `<= len`).
    pub fn floor_char_boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.len());
        while !self.rope.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// The smallest boundary offset that is `>= offset` (clamped to `len`).
    pub fn ceil_char_boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.len());
        while !self.rope.is_char_boundary(offset) {
            offset += 1;
        }
        offset
    }

    pub(crate) fn check_offset(&self, offset: usize) -> Result<(), BufferError> {
        if offset > self.len() {
            return Err(BufferError::OutOfBounds {
                offset,
                len: self.len(),
            });
        }
        if !self.rope.is_char_boundary(offset) {
            return Err(BufferError::NotCharBoundary { offset });
        }
        Ok(())
    }

    pub(crate) fn check_range(&self, range: &Range<usize>) -> Result<(), BufferError> {
        if range.start > range.end {
            return Err(BufferError::InvalidEdit {
                range: range.clone(),
                reason: "start is after end",
            });
        }
        self.check_offset(range.start)?;
        self.check_offset(range.end)
    }

    // ---- lines -----------------------------------------------------------------------------

    /// The line that contains `offset` (the position between `\r` and `\n` belongs to the line the
    /// `\r` ends).
    pub fn line_of_byte(&self, offset: usize) -> Result<usize, BufferError> {
        self.check_offset(offset)?;
        Ok(self.rope.line_of_byte(offset))
    }

    fn check_line(&self, line: usize) -> Result<(), BufferError> {
        let count = self.line_count();
        if line >= count {
            return Err(BufferError::LineOutOfBounds { line, count });
        }
        Ok(())
    }

    /// Byte offset where `line` starts.
    pub fn line_start(&self, line: usize) -> Result<usize, BufferError> {
        self.check_line(line)?;
        Ok(if line == 0 {
            0
        } else {
            self.rope.byte_of_line(line)
        })
    }

    /// The byte range of `line` including its line terminator (the last line has none).
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, BufferError> {
        let start = self.line_start(line)?;
        let end = if line + 1 < self.line_count() {
            self.rope.byte_of_line(line + 1)
        } else {
            self.len()
        };
        Ok(start..end)
    }

    /// The byte range of `line` without its terminator (`\n` or `\r\n`).
    pub fn line_content_range(&self, line: usize) -> Result<Range<usize>, BufferError> {
        let range = self.line_range(line)?;
        if range.end == self.len() && !self.ends_with_lf() {
            // The last line has no terminator.
            return Ok(range);
        }
        if range.is_empty() {
            return Ok(range);
        }
        let mut end = range.end - 1; // the '\n'
        if end > range.start && self.rope.byte(end - 1) == b'\r' {
            end -= 1;
        }
        Ok(range.start..end)
    }

    fn ends_with_lf(&self) -> bool {
        !self.is_empty() && self.rope.byte(self.len() - 1) == b'\n'
    }

    /// The text of `line` without its terminator.
    pub fn line_text(&self, line: usize) -> Result<String, BufferError> {
        self.slice(self.line_content_range(line)?)
    }

    /// `line` end without `\n` but keeping a `\r` that precedes it: the largest valid column
    /// position of the line.
    fn line_end_before_lf(&self, line: usize) -> Result<usize, BufferError> {
        let range = self.line_range(line)?;
        if range.end > range.start && self.rope.byte(range.end - 1) == b'\n' {
            Ok(range.end - 1)
        } else {
            Ok(range.end)
        }
    }

    // ---- coordinates -----------------------------------------------------------------------

    /// UTF-16 code unit offset of a byte offset. `O(log n)`.
    pub fn byte_to_utf16(&self, offset: usize) -> Result<usize, BufferError> {
        self.check_offset(offset)?;
        Ok(self.rope.utf16_code_unit_of_byte(offset))
    }

    /// Byte offset of a UTF-16 code unit offset. `O(log n)`. An offset between the two halves of
    /// a surrogate pair is an error.
    pub fn utf16_to_byte(&self, offset: usize) -> Result<usize, BufferError> {
        let len = self.utf16_len();
        if offset > len {
            return Err(BufferError::Utf16OutOfBounds { offset, len });
        }
        // crop rounds a split offset down to the start of the pair; the round trip detects it.
        let byte = self.rope.byte_of_utf16_code_unit(offset);
        if self.rope.utf16_code_unit_of_byte(byte) != offset {
            return Err(BufferError::SplitSurrogatePair { offset });
        }
        Ok(byte)
    }

    /// Number of `char`s before a byte offset. `O(n)`: the rope has no character metric, so
    /// prefer byte offsets, [`Point`] or UTF-16 in hot paths.
    pub fn byte_to_char(&self, offset: usize) -> Result<usize, BufferError> {
        self.check_offset(offset)?;
        Ok(self
            .rope
            .byte_slice(..offset)
            .chunks()
            .map(|c| c.chars().count())
            .sum())
    }

    /// Byte offset of the `char` with the given index (`char_count()` is allowed). `O(n)`.
    pub fn char_to_byte(&self, index: usize) -> Result<usize, BufferError> {
        let mut remaining = index;
        let mut base = 0;
        for chunk in self.rope.chunks() {
            let n = chunk.chars().count();
            if remaining < n {
                let within = chunk
                    .char_indices()
                    .nth(remaining)
                    .map_or(chunk.len(), |(i, _)| i);
                return Ok(base + within);
            }
            remaining -= n;
            base += chunk.len();
        }
        if remaining == 0 {
            return Ok(self.len());
        }
        Err(BufferError::CharOutOfBounds {
            offset: index,
            len: index - remaining,
        })
    }

    /// Number of `char`s in the text. `O(n)`.
    pub fn char_count(&self) -> usize {
        self.rope.chunks().map(|c| c.chars().count()).sum()
    }

    /// Byte offset → (line, column in `char`s). A position between `\r` and `\n` has the column
    /// after the `\r`.
    pub fn byte_to_point(&self, offset: usize) -> Result<Point, BufferError> {
        self.check_offset(offset)?;
        let line = self.rope.line_of_byte(offset);
        let start = self.line_start(line)?;
        let column = self
            .rope
            .byte_slice(start..offset)
            .chunks()
            .map(|c| c.chars().count())
            .sum();
        Ok(Point { line, column })
    }

    /// (line, column in `char`s) → byte offset. The largest column is the one at the end of the
    /// line content (or after a `\r` that precedes the `\n`).
    pub fn point_to_byte(&self, point: Point) -> Result<usize, BufferError> {
        let start = self.line_start(point.line)?;
        let end = self.line_end_before_lf(point.line)?;
        let mut remaining = point.column;
        let mut at = start;
        for chunk in self.rope.byte_slice(start..end).chunks() {
            let n = chunk.chars().count();
            if remaining < n {
                let within = chunk
                    .char_indices()
                    .nth(remaining)
                    .map_or(chunk.len(), |(i, _)| i);
                return Ok(at + within);
            }
            remaining -= n;
            at += chunk.len();
        }
        if remaining == 0 {
            return Ok(end);
        }
        Err(BufferError::ColumnOutOfBounds {
            line: point.line,
            column: point.column,
        })
    }

    /// Byte offset → (line, UTF-16 column), the LSP `Position` for the UTF-16 encoding.
    pub fn byte_to_utf16_point(&self, offset: usize) -> Result<Utf16Point, BufferError> {
        self.check_offset(offset)?;
        let line = self.rope.line_of_byte(offset);
        let start = self.line_start(line)?;
        let character =
            self.rope.utf16_code_unit_of_byte(offset) - self.rope.utf16_code_unit_of_byte(start);
        Ok(Utf16Point { line, character })
    }

    /// (line, UTF-16 column) → byte offset; errors when the column is past the line or splits a
    /// surrogate pair. See [`utf16_point_to_byte_clamped`](Self::utf16_point_to_byte_clamped) for
    /// positions from an untrusted peer.
    pub fn utf16_point_to_byte(&self, point: Utf16Point) -> Result<usize, BufferError> {
        let start = self.line_start(point.line)?;
        let end = self.line_end_before_lf(point.line)?;
        let start_u = self.rope.utf16_code_unit_of_byte(start);
        let end_u = self.rope.utf16_code_unit_of_byte(end);
        let target = start_u + point.character;
        if target > end_u {
            return Err(BufferError::ColumnOutOfBounds {
                line: point.line,
                column: point.character,
            });
        }
        self.utf16_to_byte(target)
    }

    /// Like [`utf16_point_to_byte`](Self::utf16_point_to_byte) but the way LSP clients treat
    /// server positions: a line past the end is the end of the text, a column past the line is the
    /// end of the line content, a column inside a surrogate pair rounds down to its start.
    pub fn utf16_point_to_byte_clamped(&self, point: Utf16Point) -> usize {
        let Ok(start) = self.line_start(point.line) else {
            return self.len();
        };
        let Ok(content) = self.line_content_range(point.line) else {
            return self.len();
        };
        let start_u = self.rope.utf16_code_unit_of_byte(start);
        let end_u = self.rope.utf16_code_unit_of_byte(content.end);
        let target = (start_u + point.character).min(end_u);
        // `crop` rounds a split offset down to the start of the pair, which is what we want.
        self.rope.byte_of_utf16_code_unit(target)
    }

    // ---- editing (crate-private: only `Buffer` mutates) -----------------------------------

    /// Checks that `edits` are ordered, non-overlapping, in bounds and on boundaries.
    pub(crate) fn validate_edits(&self, edits: &[Edit]) -> Result<(), BufferError> {
        let mut prev_end = 0;
        for edit in edits {
            let range = &edit.range;
            if range.start < prev_end {
                return Err(BufferError::InvalidEdit {
                    range: range.clone(),
                    reason: "edits overlap or are not ordered",
                });
            }
            self.check_range(range)?;
            prev_end = range.end;
        }
        Ok(())
    }

    /// Applies validated edits (last to first, so earlier offsets stay valid).
    pub(crate) fn apply_validated(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            let range = edit.range.clone();
            match (range.is_empty(), edit.text.is_empty()) {
                (true, true) => {}
                (true, false) => self.rope.insert(range.start, &*edit.text),
                (false, true) => self.rope.delete(range),
                (false, false) => self.rope.replace(range, &*edit.text),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Text {
        Text::from(s)
    }

    #[test]
    fn lines_count_the_empty_last_line() {
        assert_eq!(text("").line_count(), 1);
        assert_eq!(text("a").line_count(), 1);
        assert_eq!(text("a\n").line_count(), 2);
        assert_eq!(text("a\r\nb").line_count(), 2);
        let t = text("ab\r\ncd\n");
        assert_eq!(t.line_range(0).unwrap(), 0..4);
        assert_eq!(t.line_content_range(0).unwrap(), 0..2);
        assert_eq!(t.line_range(1).unwrap(), 4..7);
        assert_eq!(t.line_content_range(1).unwrap(), 4..6);
        assert_eq!(t.line_range(2).unwrap(), 7..7);
        assert_eq!(t.line_content_range(2).unwrap(), 7..7);
        assert!(matches!(
            t.line_range(3),
            Err(BufferError::LineOutOfBounds { line: 3, count: 3 })
        ));
        assert_eq!(t.line_text(0).unwrap(), "ab");
        assert_eq!(text("x").line_text(0).unwrap(), "x");
    }

    #[test]
    fn utf16_offsets_count_surrogate_pairs() {
        let t = text("a🦀é\r\nb");
        // a=1 byte/1 unit, 🦀=4/2, é=2/1, \r\n=2/2
        assert_eq!(t.byte_to_utf16(5).unwrap(), 3);
        assert_eq!(t.utf16_to_byte(3).unwrap(), 5);
        assert!(matches!(
            t.utf16_to_byte(2),
            Err(BufferError::SplitSurrogatePair { offset: 2 })
        ));
        assert!(matches!(
            t.byte_to_utf16(2),
            Err(BufferError::NotCharBoundary { offset: 2 })
        ));
        assert!(matches!(
            t.utf16_to_byte(99),
            Err(BufferError::Utf16OutOfBounds { .. })
        ));
    }

    #[test]
    fn points_round_trip_around_crlf() {
        let t = text("a🦀\r\nbé");
        let cr = t.line_content_range(0).unwrap().end;
        assert_eq!(t.byte_to_point(cr).unwrap(), Point { line: 0, column: 2 });
        // between \r and \n
        assert_eq!(
            t.byte_to_point(cr + 1).unwrap(),
            Point { line: 0, column: 3 }
        );
        assert_eq!(
            t.point_to_byte(Point { line: 0, column: 3 }).unwrap(),
            cr + 1
        );
        assert!(t.point_to_byte(Point { line: 0, column: 4 }).is_err());
        assert_eq!(
            t.point_to_byte(Point { line: 1, column: 2 }).unwrap(),
            t.len()
        );
        let p = t.byte_to_utf16_point(t.len()).unwrap();
        assert_eq!(
            p,
            Utf16Point {
                line: 1,
                character: 2
            }
        );
        assert_eq!(t.utf16_point_to_byte(p).unwrap(), t.len());
    }

    #[test]
    fn clamped_positions_follow_lsp() {
        let t = text("a🦀\nb");
        assert_eq!(
            t.utf16_point_to_byte_clamped(Utf16Point {
                line: 0,
                character: 99
            }),
            5
        );
        assert_eq!(
            t.utf16_point_to_byte_clamped(Utf16Point {
                line: 0,
                character: 2
            }),
            1
        );
        assert_eq!(
            t.utf16_point_to_byte_clamped(Utf16Point {
                line: 9,
                character: 0
            }),
            t.len()
        );
    }

    #[test]
    fn chars_and_bytes() {
        let t = text("a🦀é");
        assert_eq!(t.byte_to_char(5).unwrap(), 2);
        assert_eq!(t.char_to_byte(2).unwrap(), 5);
        assert_eq!(t.char_to_byte(3).unwrap(), t.len());
        assert!(t.char_to_byte(4).is_err());
        assert_eq!(t.char_count(), 3);
        assert_eq!(t.char_at(1), Some('🦀'));
        assert_eq!(t.char_before(5), Some('🦀'));
        assert_eq!(t.char_at(2), None);
        assert_eq!(t.floor_char_boundary(3), 1);
        assert_eq!(t.ceil_char_boundary(3), 5);
    }
}
