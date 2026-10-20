use std::ops::Range;
use std::sync::Arc;

use crate::anchor::{Anchor, Bias};
use crate::edit::Edit;
use crate::error::BufferError;
use crate::selection::Selections;
use crate::text::Text;

/// One edit of a [`Transaction`] seen from both sides: the bytes it replaced in the old text and
/// the bytes that replaced them in the new text. This is what an incremental parser
/// (tree-sitter's `InputEdit`) or an LSP `didChange` needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Range in the text before the transaction.
    pub old: Range<usize>,
    /// Range in the text after the transaction.
    pub new: Range<usize>,
}

/// An atomic, ordered set of edits.
///
/// The edits of a transaction are expressed in the coordinates of the text *before* it and apply
/// simultaneously: they are sorted, do not overlap (touching is fine) and are applied all or none.
/// Two insertions at the same offset keep the order they were given in.
///
/// Transactions compose ([`compose`](Self::compose)), invert ([`invert`](Self::invert)) and map
/// offsets ([`map_offset`](Self::map_offset)); the undo history is a tree of them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Transaction {
    edits: Vec<Edit>,
    /// `shifts[i]` = total length change of `edits[..i]`; one more entry than `edits`.
    shifts: Vec<isize>,
    selection_before: Option<Selections>,
    selection_after: Option<Selections>,
}

impl Transaction {
    /// The transaction that changes nothing.
    pub fn empty() -> Self {
        Self::from_sorted(Vec::new())
    }

    /// Builds a transaction from edits in any order (sorted by position, ties keep their order).
    /// No-op edits are dropped; overlapping ranges are an error.
    pub fn new(edits: impl IntoIterator<Item = Edit>) -> Result<Self, BufferError> {
        let mut edits: Vec<Edit> = edits.into_iter().filter(|e| !e.is_noop()).collect();
        for e in &edits {
            if e.range.start > e.range.end {
                return Err(BufferError::InvalidEdit {
                    range: e.range.clone(),
                    reason: "start is after end",
                });
            }
        }
        edits.sort_by_key(|e| (e.range.start, e.range.end));
        let mut prev_end = 0;
        for e in &edits {
            if e.range.start < prev_end {
                return Err(BufferError::InvalidEdit {
                    range: e.range.clone(),
                    reason: "edits overlap",
                });
            }
            prev_end = e.range.end;
        }
        Ok(Self::from_sorted(edits))
    }

    /// Replaces `range` with `text`.
    pub fn replace(range: Range<usize>, text: impl Into<Arc<str>>) -> Result<Self, BufferError> {
        Self::new([Edit::new(range, text)])
    }

    /// Inserts `text` at `offset`.
    pub fn insert(offset: usize, text: impl Into<Arc<str>>) -> Self {
        let edit = Edit::insert(offset, text);
        Self::from_sorted(if edit.is_noop() {
            Vec::new()
        } else {
            vec![edit]
        })
    }

    /// Deletes `range`.
    pub fn delete(range: Range<usize>) -> Result<Self, BufferError> {
        Self::new([Edit::delete(range)])
    }

    fn from_sorted(edits: Vec<Edit>) -> Self {
        let mut shifts = Vec::with_capacity(edits.len() + 1);
        let mut total = 0isize;
        shifts.push(0);
        for e in &edits {
            total += e.delta();
            shifts.push(total);
        }
        Self {
            edits,
            shifts,
            selection_before: None,
            selection_after: None,
        }
    }

    /// Records the selections before and after this transaction, so undo and redo can restore
    /// them (a transaction's inverse swaps the two).
    #[must_use]
    pub fn with_selections(
        mut self,
        before: Option<Selections>,
        after: Option<Selections>,
    ) -> Self {
        self.selection_before = before;
        self.selection_after = after;
        self
    }

    /// Selections that were active before the transaction, if recorded.
    pub fn selection_before(&self) -> Option<&Selections> {
        self.selection_before.as_ref()
    }

    /// Selections to show after the transaction, if recorded.
    pub fn selection_after(&self) -> Option<&Selections> {
        self.selection_after.as_ref()
    }

    /// The edits, ordered, in coordinates of the text before the transaction.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Whether the transaction changes nothing.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Change of the text length.
    pub fn len_delta(&self) -> isize {
        self.shifts.last().copied().unwrap_or(0)
    }

    /// Bytes of replacement text held (the history's size accounting).
    pub(crate) fn text_bytes(&self) -> usize {
        self.edits
            .iter()
            .map(|e| e.text.len() + std::mem::size_of::<Edit>())
            .sum()
    }

    /// Every edit as (old range, new range).
    pub fn changes(&self) -> Vec<Change> {
        self.edits
            .iter()
            .zip(&self.shifts)
            .map(|(e, shift)| {
                let start = shift_offset(e.range.start, *shift);
                Change {
                    old: e.range.clone(),
                    new: start..start + e.text.len(),
                }
            })
            .collect()
    }

    /// Where `offset` of the old text is in the new text.
    ///
    /// Offsets outside every edit shift by the edits before them. An offset on the boundary of or
    /// inside a replaced or deleted range collapses to the start of the replacement
    /// ([`Bias::Left`]) or to its end ([`Bias::Right`]); an insertion exactly at `offset` is
    /// before it for `Left` and after it for `Right`.
    pub fn map_offset(&self, offset: usize, bias: Bias) -> usize {
        let first = self.edits.partition_point(|e| e.range.end < offset);
        let touching_end = self.edits.partition_point(|e| e.range.start <= offset);
        if first >= touching_end {
            // No edit contains `offset` (closed ranges).
            return shift_offset(offset, self.shifts[first]);
        }
        match bias {
            Bias::Left => shift_offset(self.edits[first].range.start, self.shifts[first]),
            Bias::Right => {
                let last = touching_end - 1;
                shift_offset(self.edits[last].range.start, self.shifts[last])
                    + self.edits[last].text.len()
            }
        }
    }

    /// [`map_offset`](Self::map_offset) for an [`Anchor`].
    pub fn map_anchor(&self, anchor: Anchor) -> Anchor {
        Anchor {
            offset: self.map_offset(anchor.offset, anchor.bias),
            bias: anchor.bias,
        }
    }

    /// The transaction that undoes this one, given the text it applies to.
    ///
    /// The inverse's selections are this transaction's, swapped.
    pub fn invert(&self, before: &Text) -> Result<Transaction, BufferError> {
        before.validate_edits(&self.edits)?;
        let mut edits = Vec::with_capacity(self.edits.len());
        for (e, shift) in self.edits.iter().zip(&self.shifts) {
            let start = shift_offset(e.range.start, *shift);
            let original = before.slice(e.range.clone())?;
            edits.push(Edit::new(start..start + e.text.len(), original));
        }
        let mut inverse = Self::from_sorted(edits);
        inverse.selection_before = self.selection_after.clone();
        inverse.selection_after = self.selection_before.clone();
        Ok(inverse)
    }

    /// The transaction equivalent to applying `self` and then `next` (whose edits are in the
    /// coordinates `self` produces). `selection_before` is `self`'s, `selection_after` is
    /// `next`'s, or `self`'s mapped through `next` when `next` recorded none.
    pub fn compose(&self, next: &Transaction) -> Result<Transaction, BufferError> {
        let edits = compose_edits(&self.edits, &next.edits)?;
        let mut out = Self::from_sorted(edits);
        out.selection_before = self.selection_before.clone();
        out.selection_after = match (&next.selection_after, &self.selection_after) {
            (Some(after), _) => Some(after.clone()),
            (None, Some(mid)) => Some(mid.map(next)),
            (None, None) => None,
        };
        Ok(out)
    }

    /// Applies the edits to `s` (for tests and callers that hold plain strings).
    pub fn apply_to_string(&self, s: &str) -> Result<String, BufferError> {
        let mut out = String::with_capacity(s.len());
        let mut at = 0;
        for e in &self.edits {
            let (start, end) = (e.range.start, e.range.end);
            if end > s.len() {
                return Err(BufferError::OutOfBounds {
                    offset: end,
                    len: s.len(),
                });
            }
            if !s.is_char_boundary(start) {
                return Err(BufferError::NotCharBoundary { offset: start });
            }
            if !s.is_char_boundary(end) {
                return Err(BufferError::NotCharBoundary { offset: end });
            }
            out.push_str(&s[at..start]);
            out.push_str(&e.text);
            at = end;
        }
        out.push_str(&s[at..]);
        Ok(out)
    }
}

fn shift_offset(offset: usize, shift: isize) -> usize {
    // A shift is the net length change of edits before `offset`, so the result is >= 0 and
    // <= the new text length.
    offset.saturating_add_signed(shift)
}

// ---- composition ----------------------------------------------------------------------------

#[derive(Clone)]
enum Op {
    Retain(usize),
    Delete(usize),
    Insert(Arc<str>),
}

impl Op {
    fn len(&self) -> usize {
        match self {
            Op::Retain(n) | Op::Delete(n) => *n,
            Op::Insert(s) => s.len(),
        }
    }
}

enum Peek<'a> {
    Retain(usize),
    Delete(usize),
    Insert(&'a str),
}

/// Edits as a sequence of operations over the old text, with an implicit infinite retain at the
/// end, consumed piecewise.
struct Ops {
    ops: Vec<Op>,
    index: usize,
    used: usize,
}

impl Ops {
    fn new(edits: &[Edit]) -> Self {
        let mut ops = Vec::with_capacity(edits.len() * 2 + 1);
        let mut pos = 0;
        for e in edits {
            if e.range.start > pos {
                ops.push(Op::Retain(e.range.start - pos));
            }
            if !e.range.is_empty() {
                ops.push(Op::Delete(e.range.len()));
            }
            if !e.text.is_empty() {
                ops.push(Op::Insert(e.text.clone()));
            }
            pos = e.range.end;
        }
        Self {
            ops,
            index: 0,
            used: 0,
        }
    }

    fn peek(&self) -> Option<Peek<'_>> {
        Some(match self.ops.get(self.index)? {
            Op::Retain(n) => Peek::Retain(n - self.used),
            Op::Delete(n) => Peek::Delete(n - self.used),
            Op::Insert(s) => Peek::Insert(&s[self.used..]),
        })
    }

    fn take(&mut self, n: usize) {
        self.used += n;
        if self
            .ops
            .get(self.index)
            .is_some_and(|op| self.used >= op.len())
        {
            self.index += 1;
            self.used = 0;
        }
    }
}

/// Collects retain/delete/insert operations over the original text back into edits.
#[derive(Default)]
struct Builder {
    edits: Vec<Edit>,
    pos: usize,
    open: bool,
    start: usize,
    deleted: usize,
    inserted: String,
}

impl Builder {
    fn open(&mut self) {
        if !self.open {
            self.open = true;
            self.start = self.pos;
        }
    }

    fn flush(&mut self) {
        if self.open && (self.deleted > 0 || !self.inserted.is_empty()) {
            let text = std::mem::take(&mut self.inserted);
            self.edits
                .push(Edit::new(self.start..self.start + self.deleted, text));
        }
        self.open = false;
        self.deleted = 0;
        self.inserted.clear();
    }

    fn retain(&mut self, n: usize) {
        self.flush();
        self.pos += n;
    }

    fn delete(&mut self, n: usize) {
        self.open();
        self.deleted += n;
        self.pos += n;
    }

    fn insert(&mut self, s: &str) {
        self.open();
        self.inserted.push_str(s);
    }

    fn finish(mut self) -> Vec<Edit> {
        self.flush();
        self.edits
    }
}

fn compose_edits(first: &[Edit], second: &[Edit]) -> Result<Vec<Edit>, BufferError> {
    let mut a = Ops::new(first);
    let mut b = Ops::new(second);
    let mut out = Builder::default();
    let bad = |at: usize| BufferError::InvalidEdit {
        range: at..at,
        reason: "edit splits a character of the earlier edit's text",
    };
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(Peek::Delete(n)), _) => {
                out.delete(n);
                a.take(n);
            }
            (_, Some(Peek::Insert(s))) => {
                out.insert(s);
                let n = s.len();
                b.take(n);
            }
            (None, Some(Peek::Retain(m))) => {
                out.retain(m);
                b.take(m);
            }
            (None, Some(Peek::Delete(m))) => {
                out.delete(m);
                b.take(m);
            }
            (Some(Peek::Retain(n)), None) => {
                out.retain(n);
                a.take(n);
            }
            (Some(Peek::Insert(s)), None) => {
                out.insert(s);
                let n = s.len();
                a.take(n);
            }
            (Some(Peek::Retain(n)), Some(Peek::Retain(m))) => {
                let k = n.min(m);
                out.retain(k);
                a.take(k);
                b.take(k);
            }
            (Some(Peek::Retain(n)), Some(Peek::Delete(m))) => {
                let k = n.min(m);
                out.delete(k);
                a.take(k);
                b.take(k);
            }
            (Some(Peek::Insert(s)), Some(Peek::Retain(m))) => {
                let k = s.len().min(m);
                out.insert(s.get(..k).ok_or_else(|| bad(k))?);
                a.take(k);
                b.take(k);
            }
            (Some(Peek::Insert(s)), Some(Peek::Delete(m))) => {
                let k = s.len().min(m);
                if !s.is_char_boundary(k) {
                    return Err(bad(k));
                }
                a.take(k);
                b.take(k);
            }
        }
    }
    Ok(out.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(edits: &[(usize, usize, &str)]) -> Transaction {
        Transaction::new(edits.iter().map(|(s, e, t)| Edit::new(*s..*e, *t))).unwrap()
    }

    #[test]
    fn new_sorts_and_rejects_overlap() {
        let t = tx(&[(5, 6, "x"), (0, 1, "y")]);
        assert_eq!(t.edits()[0].range, 0..1);
        assert!(Transaction::new([Edit::new(0..3, "a"), Edit::new(2..4, "b")]).is_err());
        // touching and same-offset insertions are fine and keep their order
        let t = Transaction::new([
            Edit::insert(2, "a"),
            Edit::insert(2, "b"),
            Edit::new(0..2, ""),
        ])
        .unwrap();
        assert_eq!(t.apply_to_string("0123").unwrap(), "ab23");
    }

    #[test]
    fn map_offset_biases() {
        // "abcdef": replace "cd" (2..4) with "XYZ"
        let t = tx(&[(2, 4, "XYZ")]);
        assert_eq!(t.map_offset(1, Bias::Left), 1);
        assert_eq!(t.map_offset(2, Bias::Left), 2);
        assert_eq!(t.map_offset(3, Bias::Left), 2);
        assert_eq!(t.map_offset(3, Bias::Right), 5);
        assert_eq!(t.map_offset(4, Bias::Right), 5);
        assert_eq!(t.map_offset(5, Bias::Left), 6);
        // insertion at the offset
        let t = Transaction::insert(3, "__");
        assert_eq!(t.map_offset(3, Bias::Left), 3);
        assert_eq!(t.map_offset(3, Bias::Right), 5);
        assert_eq!(t.map_offset(4, Bias::Left), 6);
    }

    #[test]
    fn map_offset_chains_touching_edits() {
        // delete 3..5, insert at 5, delete 5..7: an offset at 5 is inside the whole chain.
        let t = Transaction::new([
            Edit::delete(3..5),
            Edit::insert(5, "ab"),
            Edit::delete(5..7),
        ])
        .unwrap();
        assert_eq!(t.map_offset(5, Bias::Left), 3);
        assert_eq!(t.map_offset(5, Bias::Right), 5);
        assert_eq!(t.map_offset(8, Bias::Left), 6);
    }

    #[test]
    fn compose_matches_sequential_application() {
        let a = tx(&[(1, 2, "XX"), (4, 4, "!")]);
        let b = tx(&[(0, 1, "<"), (2, 5, ""), (6, 6, ">")]);
        let s = "abcdef";
        let seq = b.apply_to_string(&a.apply_to_string(s).unwrap()).unwrap();
        let both = a.compose(&b).unwrap();
        assert_eq!(both.apply_to_string(s).unwrap(), seq);
    }

    #[test]
    fn compose_merges_adjacent_typing() {
        let a = Transaction::insert(3, "a");
        let b = Transaction::insert(4, "b");
        let c = a.compose(&b).unwrap();
        assert_eq!(c.edits(), &[Edit::insert(3, "ab")]);
    }

    #[test]
    fn invert_restores_text() {
        let text = Text::from("héllo wörld");
        let t = tx(&[(0, 1, "J"), (8, 10, "o")]);
        let inv = t.invert(&text).unwrap();
        let after = t.apply_to_string("héllo wörld").unwrap();
        assert_eq!(inv.apply_to_string(&after).unwrap(), "héllo wörld");
    }
}
