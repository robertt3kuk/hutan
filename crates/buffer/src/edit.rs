use std::ops::Range;
use std::sync::Arc;

/// Replace the bytes of `range` with `text` (an insertion has an empty range, a deletion empty
/// text). Ranges are byte offsets into the text the edit is applied to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// The bytes replaced.
    pub range: Range<usize>,
    /// The replacement.
    pub text: Arc<str>,
}

impl Edit {
    /// Replaces `range` with `text`.
    pub fn new(range: Range<usize>, text: impl Into<Arc<str>>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }

    /// Inserts `text` at `offset`.
    pub fn insert(offset: usize, text: impl Into<Arc<str>>) -> Self {
        Self::new(offset..offset, text)
    }

    /// Deletes `range`.
    pub fn delete(range: Range<usize>) -> Self {
        Self::new(range, "")
    }

    /// Change of the text length this edit makes.
    pub fn delta(&self) -> isize {
        // Text lengths are far below `isize::MAX` (an allocation cannot exceed it).
        self.text.len() as isize - self.range.len() as isize
    }

    pub(crate) fn is_noop(&self) -> bool {
        self.range.is_empty() && self.text.is_empty()
    }
}
