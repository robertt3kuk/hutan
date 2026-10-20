use hutan_buffer::{BufferError, Point, Text, Utf16Point};
use proptest::prelude::*;

use crate::common::{boundaries, config, text};

proptest! {
    #![proptest_config(config())]

    /// Every coordinate system round-trips at every character boundary and agrees with `std`,
    /// across emoji, combining marks, CRLF and lone CRs.
    #[test]
    fn coordinates_round_trip(s in text(200)) {
        let t = Text::from(s.as_str());
        for b in boundaries(&s) {
            let prefix = &s[..b];
            // UTF-16
            let u = t.byte_to_utf16(b).unwrap();
            prop_assert_eq!(u, prefix.encode_utf16().count());
            prop_assert_eq!(t.utf16_to_byte(u).unwrap(), b);
            // chars
            let c = t.byte_to_char(b).unwrap();
            prop_assert_eq!(c, prefix.chars().count());
            prop_assert_eq!(t.char_to_byte(c).unwrap(), b);
            // (line, column)
            let p = t.byte_to_point(b).unwrap();
            let line = prefix.matches('\n').count();
            let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
            prop_assert_eq!(p, Point { line, column: prefix[line_start..].chars().count() });
            prop_assert_eq!(t.point_to_byte(p).unwrap(), b);
            prop_assert_eq!(t.line_of_byte(b).unwrap(), line);
            // (line, UTF-16 column), the LSP position
            let lp = t.byte_to_utf16_point(b).unwrap();
            prop_assert_eq!(lp, Utf16Point { line, character: prefix[line_start..].encode_utf16().count() });
            prop_assert_eq!(t.utf16_point_to_byte(lp).unwrap(), b);
            // Clamped positions stop at the end of the line content, before a \r\n.
            prop_assert_eq!(t.utf16_point_to_byte_clamped(lp), b.min(t.line_content_range(line).unwrap().end));
        }
    }

    /// Offsets that are not boundaries are errors, never panics, and UTF-16 offsets that split a
    /// surrogate pair are refused (or round down when clamped).
    #[test]
    fn invalid_offsets_are_errors(s in text(60)) {
        let t = Text::from(s.as_str());
        for b in 0..=s.len() {
            if !s.is_char_boundary(b) {
                assert!(matches!(t.byte_to_utf16(b), Err(BufferError::NotCharBoundary { .. })));
                assert!(matches!(t.byte_to_point(b), Err(BufferError::NotCharBoundary { .. })));
            }
        }
        prop_assert!(t.byte_to_utf16(s.len() + 1).is_err());
        let units: Vec<u16> = s.encode_utf16().collect();
        for u in 0..=units.len() {
            let splits = u > 0 && u < units.len() && (0xD800..0xDC00).contains(&units[u - 1]) && (0xDC00..0xE000).contains(&units[u]);
            match t.utf16_to_byte(u) {
                Ok(b) => {
                    prop_assert!(!splits);
                    prop_assert_eq!(String::from_utf16(&units[..u]).unwrap().len(), b);
                }
                Err(BufferError::SplitSurrogatePair { .. }) => prop_assert!(splits),
                Err(e) => prop_assert!(false, "unexpected {e}"),
            }
        }
        prop_assert!(t.utf16_to_byte(units.len() + 1).is_err());
    }

    /// Line ranges tile the text and agree with `str::split`.
    #[test]
    fn line_ranges_tile_the_text(s in text(120)) {
        let t = Text::from(s.as_str());
        let lines: Vec<&str> = s.split('\n').collect();
        prop_assert_eq!(t.line_count(), lines.len());
        let mut at = 0;
        for (i, expected) in lines.iter().enumerate() {
            let range = t.line_range(i).unwrap();
            prop_assert_eq!(range.start, at);
            at = range.end;
            let content = t.line_content_range(i).unwrap();
            let expected_content = expected.strip_suffix('\r').filter(|_| i + 1 < lines.len()).unwrap_or(expected);
            prop_assert_eq!(&s[content.clone()], expected_content);
            prop_assert_eq!(t.line_text(i).unwrap(), expected_content);
            prop_assert!(content.start == range.start && content.end <= range.end);
        }
        prop_assert_eq!(at, s.len());
        prop_assert!(t.line_range(lines.len()).is_err());
    }
}

#[test]
fn crop_rounds_a_split_utf16_offset_down() {
    // Guards the assumption `Text::utf16_to_byte` relies on (see the exact `crop` pin).
    let t = Text::from("a🦀b");
    assert!(matches!(
        t.utf16_to_byte(2),
        Err(BufferError::SplitSurrogatePair { offset: 2 })
    ));
    assert_eq!(
        t.utf16_point_to_byte_clamped(Utf16Point {
            line: 0,
            character: 2
        }),
        1
    );
}

#[test]
fn emoji_zwj_and_combining_marks_count_by_code_unit() {
    let s = "👩‍👩‍👧e\u{301}\r\nx";
    let t = Text::from(s);
    assert_eq!(t.utf16_len(), s.encode_utf16().count());
    let end_of_family = "👩‍👩‍👧".len();
    assert_eq!(
        t.byte_to_utf16(end_of_family).unwrap(),
        "👩‍👩‍👧".encode_utf16().count()
    );
    // Combining marks are separate scalar values and separate columns.
    let after_e = end_of_family + 1;
    assert_eq!(
        t.byte_to_point(after_e).unwrap().column,
        "👩‍👩‍👧e".chars().count()
    );
}
