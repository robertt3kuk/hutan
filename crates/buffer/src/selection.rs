use crate::anchor::Bias;
use crate::error::BufferError;
use crate::text::Text;
use crate::transaction::Transaction;

/// Most ranges one [`Selections`] may hold (bounded like everything in Hutan).
pub const MAX_SELECTIONS: usize = 100_000;

/// One selection range: `anchor` is the fixed end, `head` the end that moves (the caret). They
/// may be equal (a cursor) and `head` may be before `anchor` (a backward selection).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Selection {
    /// The fixed end, as a byte offset.
    pub anchor: usize,
    /// The moving end (caret), as a byte offset.
    pub head: usize,
}

impl Selection {
    /// A range from `anchor` to `head`.
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// An empty range (a caret) at `offset`.
    pub fn cursor(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    /// The smaller end.
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The larger end.
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// Whether the range selects nothing.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Whether the head is before the anchor.
    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// The selected bytes.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start()..self.end()
    }

    fn from_ordered(start: usize, end: usize, reversed: bool) -> Self {
        if reversed {
            Self::new(end, start)
        } else {
            Self::new(start, end)
        }
    }

    /// This range after `transaction`. A caret moves after text inserted at it; a selection
    /// keeps text inserted at its ends outside, and a selection whose text was replaced collapses
    /// to a caret after the replacement.
    pub fn map(&self, transaction: &Transaction) -> Self {
        if self.is_empty() {
            return Self::cursor(transaction.map_offset(self.head, Bias::Right));
        }
        let start = transaction.map_offset(self.start(), Bias::Right);
        let end = transaction.map_offset(self.end(), Bias::Left);
        if start > end {
            return Self::cursor(start);
        }
        Self::from_ordered(start, end, self.is_reversed())
    }
}

/// An ordered set of non-overlapping [`Selection`]s (multi-cursor) with one primary.
///
/// Invariants, kept by every constructor and mutator: at least one range, at most
/// [`MAX_SELECTIONS`]; sorted by position; no two ranges overlap, and a caret touching another
/// range is merged into it (two non-empty ranges that merely touch stay separate). When ranges are
/// merged the result is primary if either was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    ranges: Vec<Selection>,
    primary: usize,
}

impl Selections {
    /// Builds a set from ranges in any order; `primary` indexes the *given* order. Overlapping
    /// ranges are merged.
    pub fn new(
        ranges: impl IntoIterator<Item = Selection>,
        primary: usize,
    ) -> Result<Self, BufferError> {
        let ranges: Vec<Selection> = ranges.into_iter().collect();
        if ranges.is_empty() || ranges.len() > MAX_SELECTIONS {
            return Err(BufferError::SelectionCount {
                got: ranges.len(),
                max: MAX_SELECTIONS,
            });
        }
        if primary >= ranges.len() {
            return Err(BufferError::PrimaryOutOfRange {
                primary,
                count: ranges.len(),
            });
        }
        Ok(Self::normalized(
            ranges
                .into_iter()
                .enumerate()
                .map(|(i, r)| (r, i == primary))
                .collect(),
        ))
    }

    /// One range.
    pub fn single(range: Selection) -> Self {
        Self {
            ranges: vec![range],
            primary: 0,
        }
    }

    /// One caret.
    pub fn cursor(offset: usize) -> Self {
        Self::single(Selection::cursor(offset))
    }

    fn normalized(mut tagged: Vec<(Selection, bool)>) -> Self {
        tagged.sort_by_key(|(r, _)| (r.start(), r.end()));
        let mut ranges: Vec<Selection> = Vec::with_capacity(tagged.len());
        let mut primary = 0;
        let mut last_is_primary = false;
        for (cur, is_primary) in tagged {
            if let Some(prev) = ranges.last().copied() {
                let touching = cur.start() == prev.end() && (cur.is_empty() || prev.is_empty());
                if cur.start() < prev.end() || touching {
                    let reversed = if is_primary {
                        cur.is_reversed()
                    } else if last_is_primary || !prev.is_empty() {
                        prev.is_reversed()
                    } else {
                        cur.is_reversed()
                    };
                    let end = prev.end().max(cur.end());
                    if let Some(slot) = ranges.last_mut() {
                        *slot = Selection::from_ordered(prev.start(), end, reversed);
                    }
                    last_is_primary |= is_primary;
                    if last_is_primary {
                        primary = ranges.len() - 1;
                    }
                    continue;
                }
            }
            ranges.push(cur);
            last_is_primary = is_primary;
            if is_primary {
                primary = ranges.len() - 1;
            }
        }
        Self { ranges, primary }
    }

    /// The ranges in document order.
    pub fn ranges(&self) -> &[Selection] {
        &self.ranges
    }

    /// Iterates the ranges in document order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Selection> {
        self.ranges.iter()
    }

    /// Number of ranges (at least 1).
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Always `false`: a set holds at least one range. Present for API symmetry.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Index of the primary range in [`ranges`](Self::ranges).
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// The primary range.
    pub fn primary(&self) -> Selection {
        self.ranges[self.primary]
    }

    /// Makes the range at `index` primary.
    pub fn set_primary(&mut self, index: usize) -> Result<(), BufferError> {
        if index >= self.ranges.len() {
            return Err(BufferError::PrimaryOutOfRange {
                primary: index,
                count: self.ranges.len(),
            });
        }
        self.primary = index;
        Ok(())
    }

    /// Adds a range (merging it into any it overlaps) and optionally makes it primary.
    pub fn push(&mut self, range: Selection, make_primary: bool) -> Result<(), BufferError> {
        if self.ranges.len() >= MAX_SELECTIONS {
            return Err(BufferError::SelectionCount {
                got: self.ranges.len() + 1,
                max: MAX_SELECTIONS,
            });
        }
        let primary = self.primary;
        let mut tagged: Vec<(Selection, bool)> = self
            .ranges
            .iter()
            .enumerate()
            .map(|(i, r)| (*r, i == primary && !make_primary))
            .collect();
        tagged.push((range, make_primary));
        *self = Self::normalized(tagged);
        Ok(())
    }

    /// Removes the range at `index` unless it is the only one; the primary moves to the previous
    /// range when it was removed.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.ranges.len() <= 1 || index >= self.ranges.len() {
            return false;
        }
        self.ranges.remove(index);
        if self.primary > index || self.primary >= self.ranges.len() {
            self.primary = self.primary.saturating_sub(1);
        }
        true
    }

    /// The set after `transaction`: every endpoint mapped ([`Selection::map`]), then re-merged
    /// because collapsed ranges can now touch.
    pub fn map(&self, transaction: &Transaction) -> Selections {
        let primary = self.primary;
        Self::normalized(
            self.ranges
                .iter()
                .enumerate()
                .map(|(i, r)| (r.map(transaction), i == primary))
                .collect(),
        )
    }

    /// The set with every endpoint moved to a valid offset of `text` (past-the-end offsets to the
    /// end, offsets inside a character to its start).
    pub fn clamp(&self, text: &Text) -> Selections {
        let fix = |o: usize| text.floor_char_boundary(o);
        let primary = self.primary;
        Self::normalized(
            self.ranges
                .iter()
                .enumerate()
                .map(|(i, r)| (Selection::new(fix(r.anchor), fix(r.head)), i == primary))
                .collect(),
        )
    }

    /// Whether every endpoint is a valid offset of `text`.
    pub fn is_valid_for(&self, text: &Text) -> bool {
        self.ranges
            .iter()
            .all(|r| text.is_char_boundary(r.anchor) && text.is_char_boundary(r.head))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Edit;

    fn sel(a: usize, h: usize) -> Selection {
        Selection::new(a, h)
    }

    #[test]
    fn sorts_and_merges_overlaps() {
        let s = Selections::new([sel(10, 12), sel(0, 3), sel(2, 5), sel(5, 5)], 0).unwrap();
        assert_eq!(s.ranges(), &[sel(0, 5), sel(10, 12)]);
        // The primary was the (10,12) range given first.
        assert_eq!(s.primary(), sel(10, 12));
    }

    #[test]
    fn touching_nonempty_ranges_stay_separate_but_a_caret_merges() {
        let s = Selections::new([sel(0, 3), sel(3, 6)], 1).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.primary(), sel(3, 6));
        let s = Selections::new([sel(0, 3), sel(3, 3)], 1).unwrap();
        assert_eq!(s.ranges(), &[sel(0, 3)]);
        assert_eq!(s.primary_index(), 0);
    }

    #[test]
    fn merged_range_keeps_the_primary_direction() {
        let s = Selections::new([sel(0, 4), sel(6, 2)], 1).unwrap();
        assert_eq!(s.ranges(), &[sel(6, 0)]);
    }

    #[test]
    fn rejects_empty_and_bad_primary() {
        assert!(Selections::new([], 0).is_err());
        assert!(Selections::new([sel(0, 0)], 1).is_err());
    }

    #[test]
    fn push_remove_primary() {
        let mut s = Selections::cursor(5);
        s.push(sel(1, 2), true).unwrap();
        assert_eq!(s.ranges(), &[sel(1, 2), sel(5, 5)]);
        assert_eq!(s.primary(), sel(1, 2));
        assert!(s.remove(0));
        assert_eq!(s.primary(), sel(5, 5));
        assert!(!s.remove(0));
    }

    #[test]
    fn map_through_typing_and_deleting() {
        let s = Selections::new([sel(2, 2), sel(5, 8)], 0).unwrap();
        // "x" typed at both carets' positions and at the start of the selection
        let t = Transaction::new([Edit::insert(2, "x"), Edit::insert(5, "yy")]).unwrap();
        let m = s.map(&t);
        assert_eq!(m.ranges(), &[sel(3, 3), sel(8, 11)]);
        // deleting everything under a selection collapses it
        let t = Transaction::delete(4..9).unwrap();
        let m = s.map(&t);
        assert_eq!(m.ranges(), &[sel(2, 2), sel(4, 4)]);
    }

    #[test]
    fn clamp_fixes_offsets() {
        let text = Text::from("a🦀b");
        let s = Selections::new([sel(2, 99)], 0).unwrap().clamp(&text);
        assert_eq!(s.ranges(), &[sel(1, text.len())]);
        assert!(s.is_valid_for(&text));
    }
}
