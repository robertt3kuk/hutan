//! LSP positions ↔ byte offsets of a [`Text`].
//!
//! A position is a zero-based line and a `character` counted in the encoding negotiated at
//! `initialize` (LSP 3.17 `positionEncoding`): the client offers UTF-8 first and UTF-16 second;
//! a server that does not answer uses UTF-16, the protocol's default. UTF-16 columns go through
//! `hutan-buffer`'s UTF-16 metric; UTF-8 columns are byte distances from the line start.

use hutan_buffer::{BufferError, Text, Utf16Point};
use ls_types::{Position, PositionEncodingKind, Range};

/// How a position's `character` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionEncoding {
    /// UTF-8 code units (bytes) from the start of the line.
    Utf8,
    /// UTF-16 code units from the start of the line (LSP's default).
    Utf16,
}

impl PositionEncoding {
    /// What the client offers in `general.positionEncodings`, preferred first.
    pub const OFFERED: [PositionEncoding; 2] = [PositionEncoding::Utf8, PositionEncoding::Utf16];

    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
        }
    }

    /// The encoding the server chose (`capabilities.positionEncoding`); `None` (not answered)
    /// is UTF-16. `Err` carries a choice that was not offered (`utf-32`, anything else).
    pub fn negotiate(chosen: Option<&PositionEncodingKind>) -> Result<Self, String> {
        match chosen {
            None => Ok(Self::Utf16),
            Some(kind) if *kind == PositionEncodingKind::UTF16 => Ok(Self::Utf16),
            Some(kind) if *kind == PositionEncodingKind::UTF8 => Ok(Self::Utf8),
            Some(other) => Err(other.as_str().to_owned()),
        }
    }
}

fn to_u32(value: usize) -> u32 {
    // A text has far fewer than 2^32 lines and columns (documents are capped well below 4 GiB).
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The position of byte `offset` (on a `char` boundary) in `text`.
pub fn position(
    text: &Text,
    offset: usize,
    encoding: PositionEncoding,
) -> Result<Position, BufferError> {
    match encoding {
        PositionEncoding::Utf16 => {
            let point = text.byte_to_utf16_point(offset)?;
            Ok(Position::new(to_u32(point.line), to_u32(point.character)))
        }
        PositionEncoding::Utf8 => {
            if !text.is_char_boundary(offset) {
                // `byte_to_utf16_point` reports the precise error (out of range or mid-char).
                text.byte_to_utf16_point(offset)?;
            }
            let line = text.line_of_byte(offset)?;
            let start = text.line_start(line)?;
            Ok(Position::new(to_u32(line), to_u32(offset - start)))
        }
    }
}

/// The range of the bytes `start..end` of `text`.
pub fn range(
    text: &Text,
    start: usize,
    end: usize,
    encoding: PositionEncoding,
) -> Result<Range, BufferError> {
    Ok(Range::new(
        position(text, start, encoding)?,
        position(text, end, encoding)?,
    ))
}

/// The byte offset of a position from the server, clamped the way LSP clients treat server
/// positions: a line past the end is the end of the text, a column past the line is the end of
/// the line's content, a column inside a character rounds down to its start.
pub fn offset(text: &Text, position: Position, encoding: PositionEncoding) -> usize {
    let line = position.line as usize;
    let character = position.character as usize;
    match encoding {
        PositionEncoding::Utf16 => text.utf16_point_to_byte_clamped(Utf16Point { line, character }),
        PositionEncoding::Utf8 => {
            let Ok(content) = text.line_content_range(line) else {
                return text.len();
            };
            let byte = content.start.saturating_add(character).min(content.end);
            text.floor_char_boundary(byte)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_prefers_what_the_server_chose_and_defaults_to_utf16() {
        assert_eq!(
            PositionEncoding::negotiate(None),
            Ok(PositionEncoding::Utf16)
        );
        assert_eq!(
            PositionEncoding::negotiate(Some(&PositionEncodingKind::UTF8)),
            Ok(PositionEncoding::Utf8)
        );
        assert_eq!(
            PositionEncoding::negotiate(Some(&PositionEncodingKind::UTF32)),
            Err("utf-32".to_owned())
        );
    }

    #[test]
    fn utf16_positions_count_surrogate_pairs_and_cjk() {
        // 😀 is 4 bytes / 2 UTF-16 units, 漢 and 字 3 bytes / 1 unit each.
        let text = Text::from("ok\n😀 漢字 error 🎉x\n");
        let line = "😀 漢字 error 🎉x";
        let at = |needle: &str| 3 + line.find(needle).unwrap();
        let cases = [("漢", 3), ("字", 4), ("error", 6), ("🎉", 12), ("x", 14)];
        for (needle, character) in cases {
            let pos = position(&text, at(needle), PositionEncoding::Utf16).unwrap();
            assert_eq!(pos, Position::new(1, character), "{needle}");
            assert_eq!(offset(&text, pos, PositionEncoding::Utf16), at(needle));
        }
        // UTF-8 columns are byte distances.
        let pos = position(&text, at("error"), PositionEncoding::Utf8).unwrap();
        assert_eq!(pos, Position::new(1, 12));
        assert_eq!(offset(&text, pos, PositionEncoding::Utf8), at("error"));
        // A column inside the emoji rounds down to its start, in both encodings.
        assert_eq!(
            offset(&text, Position::new(1, 1), PositionEncoding::Utf16),
            3
        );
        assert_eq!(
            offset(&text, Position::new(1, 2), PositionEncoding::Utf8),
            3
        );
        // Past the line: end of the line's content; past the text: its end.
        assert_eq!(
            offset(&text, Position::new(1, 99), PositionEncoding::Utf16),
            3 + line.len()
        );
        assert_eq!(
            offset(&text, Position::new(1, 99), PositionEncoding::Utf8),
            3 + line.len()
        );
        assert_eq!(
            offset(&text, Position::new(9, 0), PositionEncoding::Utf8),
            text.len()
        );
        // Offsets inside a character are refused.
        assert!(position(&text, 4, PositionEncoding::Utf8).is_err());
        assert!(position(&text, 4, PositionEncoding::Utf16).is_err());
    }
}
