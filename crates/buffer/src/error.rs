use std::ops::Range;

/// Everything the buffer can refuse, as a value (the crate never panics on caller input).
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    /// A byte offset past the end of the text.
    #[error("offset {offset} is past the end of the text ({len} bytes)")]
    OutOfBounds {
        /// The offending offset.
        offset: usize,
        /// The text length it was checked against.
        len: usize,
    },
    /// A byte offset inside a multi-byte character.
    #[error("offset {offset} is not on a character boundary")]
    NotCharBoundary {
        /// The offending offset.
        offset: usize,
    },
    /// A UTF-16 offset between the two halves of a surrogate pair.
    #[error("UTF-16 offset {offset} splits a surrogate pair")]
    SplitSurrogatePair {
        /// The offending UTF-16 offset.
        offset: usize,
    },
    /// A UTF-16 offset past the end of the text.
    #[error("UTF-16 offset {offset} is past the end of the text ({len} code units)")]
    Utf16OutOfBounds {
        /// The offending offset.
        offset: usize,
        /// The UTF-16 length of the text.
        len: usize,
    },
    /// A character offset past the end of the text.
    #[error("character offset {offset} is past the end of the text ({len} characters)")]
    CharOutOfBounds {
        /// The offending offset.
        offset: usize,
        /// The number of characters in the text.
        len: usize,
    },
    /// A line index past the last line.
    #[error("line {line} does not exist (the text has {count} lines)")]
    LineOutOfBounds {
        /// The offending line.
        line: usize,
        /// The number of lines.
        count: usize,
    },
    /// A column past the end of its line.
    #[error("column {column} is past the end of line {line}")]
    ColumnOutOfBounds {
        /// The line.
        line: usize,
        /// The offending column.
        column: usize,
    },
    /// Edits that are unordered, overlapping, out of bounds or split a character.
    #[error("invalid edit {range:?}: {reason}")]
    InvalidEdit {
        /// The range of the offending edit.
        range: Range<usize>,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The bytes contain NUL: not text.
    #[error("the content is binary (a NUL byte at offset {offset})")]
    Binary {
        /// Offset of the first NUL byte.
        offset: usize,
    },
    /// The bytes are not UTF-8.
    #[error("the content is not valid UTF-8 (first invalid byte at offset {valid_up_to})")]
    InvalidUtf8 {
        /// Length of the valid UTF-8 prefix.
        valid_up_to: usize,
    },
    /// More bytes than the caller's limit.
    #[error("the content is larger than the limit of {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded.
        limit: usize,
    },
    /// Reading the caller's reader failed.
    #[error("reading the content failed: {0}")]
    Io(#[from] std::io::Error),
    /// A selection set with no range, or more than [`MAX_SELECTIONS`](crate::MAX_SELECTIONS).
    #[error("a selection set needs between 1 and {max} ranges, got {got}")]
    SelectionCount {
        /// The number of ranges given.
        got: usize,
        /// The maximum.
        max: usize,
    },
    /// The primary selection index does not point at a range.
    #[error("primary selection index {primary} is out of range for {count} selections")]
    PrimaryOutOfRange {
        /// The index given.
        primary: usize,
        /// The number of ranges.
        count: usize,
    },
    /// More tracked anchors than [`MAX_ANCHORS`](crate::MAX_ANCHORS).
    #[error("too many tracked anchors (the limit is {max})")]
    TooManyAnchors {
        /// The maximum.
        max: usize,
    },
    /// A search pattern that is empty, too long or does not compile within the size limits.
    #[error("invalid search pattern: {0}")]
    Pattern(String),
}
