use hutan_buffer::{Buffer, BufferError, LineEnding, normalize_newlines};
use proptest::prelude::*;

use crate::common::{config, text_without_cr};

proptest! {
    #![proptest_config(config())]

    /// A file in one line-ending style, with or without a BOM, is written back byte for byte.
    #[test]
    fn files_round_trip_byte_for_byte(body in text_without_cr(80), crlf in any::<bool>(), bom in any::<bool>()) {
        let mut bytes = Vec::new();
        if bom {
            bytes.extend_from_slice(b"\xEF\xBB\xBF");
        }
        let content = if crlf { body.replace('\n', "\r\n") } else { body.clone() };
        bytes.extend_from_slice(content.as_bytes());

        let buffer = Buffer::from_bytes(&bytes).unwrap();
        prop_assert_eq!(buffer.text().to_string(), body.clone());
        prop_assert_eq!(buffer.has_bom(), bom);
        if body.contains('\n') {
            prop_assert_eq!(buffer.line_ending(), if crlf { LineEnding::CrLf } else { LineEnding::Lf });
        }
        prop_assert!(!buffer.has_mixed_line_endings());
        prop_assert_eq!(buffer.to_bytes(), bytes.clone());
        let mut out = Vec::new();
        buffer.write_to(&mut out).unwrap();
        prop_assert_eq!(out, bytes);
        prop_assert!(!buffer.is_dirty());
    }
}

#[test]
fn crlf_is_preserved_through_edits() {
    let mut buffer = Buffer::from_bytes(b"a\r\nb\r\n").unwrap();
    assert_eq!(buffer.text().to_string(), "a\nb\n");
    assert_eq!(buffer.line_ending(), LineEnding::CrLf);
    buffer.insert(1, "\nx").unwrap();
    assert_eq!(buffer.to_bytes(), b"a\r\nx\r\nb\r\n");
}

#[test]
fn mixed_endings_take_the_majority_and_are_reported() {
    let buffer = Buffer::from_bytes(b"a\r\nb\r\nc\nd").unwrap();
    assert!(buffer.has_mixed_line_endings());
    assert_eq!(buffer.line_ending(), LineEnding::CrLf);
    assert_eq!(buffer.to_bytes(), b"a\r\nb\r\nc\r\nd");
    let buffer = Buffer::from_bytes(b"a\nb\nc\r\nd").unwrap();
    assert_eq!(buffer.line_ending(), LineEnding::Lf);
}

#[test]
fn a_lone_cr_is_an_ordinary_character() {
    let buffer = Buffer::from_bytes(b"a\rb\r\r\nc").unwrap();
    assert_eq!(buffer.text().to_string(), "a\rb\r\nc");
    assert_eq!(buffer.to_bytes(), b"a\rb\r\r\nc");
    assert_eq!(buffer.text().line_count(), 2);
}

#[test]
fn binary_and_invalid_utf8_are_refused() {
    assert!(matches!(
        Buffer::from_bytes(b"ab\0cd"),
        Err(BufferError::Binary { offset: 2 })
    ));
    assert!(matches!(
        Buffer::from_bytes(&[0x89, b'P', b'N', b'G', 0, 1]),
        Err(BufferError::Binary { offset: 4 })
    ));
    assert!(matches!(
        Buffer::from_bytes(b"ok\xFFbad"),
        Err(BufferError::InvalidUtf8 { valid_up_to: 2 })
    ));
    // UTF-16 text has NULs
    assert!(matches!(
        Buffer::from_bytes(b"\xFF\xFEa\0b\0"),
        Err(BufferError::Binary { .. })
    ));
    let mut buffer = Buffer::from_content("keep");
    assert!(buffer.apply_external_bytes(b"x\0y").is_err());
    assert_eq!(buffer.text().to_string(), "keep");
}

#[test]
fn from_reader_enforces_the_limit() {
    let data = [b'a'; 100];
    assert!(Buffer::from_reader(&data[..], 100).is_ok());
    assert!(matches!(
        Buffer::from_reader(&data[..], 99),
        Err(BufferError::TooLarge { limit: 99 })
    ));
    struct Failing;
    impl std::io::Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
    }
    assert!(matches!(
        Buffer::from_reader(Failing, 10),
        Err(BufferError::Io(_))
    ));
}

#[test]
fn changing_the_format_makes_the_buffer_dirty_until_saved() {
    let mut buffer = Buffer::from_bytes(b"a\nb").unwrap();
    buffer.set_line_ending(LineEnding::CrLf);
    assert!(buffer.is_dirty());
    assert_eq!(buffer.to_bytes(), b"a\r\nb");
    buffer.mark_saved();
    assert!(!buffer.is_dirty());
    buffer.set_bom(true);
    assert!(buffer.is_dirty());
    assert!(buffer.to_bytes().starts_with(b"\xEF\xBB\xBF"));
}

#[test]
fn normalize_newlines_only_touches_crlf() {
    assert_eq!(normalize_newlines("a\r\nb\rc\n"), "a\nb\rc\n");
    assert!(matches!(
        normalize_newlines("plain"),
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn the_version_counts_every_change() {
    let mut buffer = Buffer::from_content("");
    assert_eq!(buffer.version(), 0);
    buffer.insert(0, "a").unwrap();
    buffer.insert(1, "b").unwrap();
    assert_eq!(buffer.version(), 2);
    buffer.undo().unwrap();
    assert_eq!(buffer.version(), 3);
    buffer.redo().unwrap();
    assert_eq!(buffer.version(), 4);
    let snapshot = buffer.snapshot();
    buffer.insert(0, "z").unwrap();
    assert_eq!(snapshot.to_string(), "ab");
    assert_eq!(buffer.text().to_string(), "zab");
}
