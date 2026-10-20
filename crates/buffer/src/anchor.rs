use crate::transaction::Transaction;

/// Which side of an insertion at its own offset an anchor sticks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bias {
    /// Stays before text inserted at its offset (sticks to the character before).
    #[default]
    Left,
    /// Moves after text inserted at its offset (sticks to the character after).
    Right,
}

/// A byte offset that follows its text through edits.
///
/// An anchor is a plain value; [`Transaction::map_anchor`] (or [`Anchor::map`]) moves it through
/// one transaction. [`Buffer`](crate::Buffer) can also track anchors for you
/// ([`track_anchor`](crate::Buffer::track_anchor)) and maps them on every edit, undo, redo and
/// external change.
///
/// An anchor inside a replaced or deleted region collapses to one end of the replacement
/// according to its bias; it does not come back when the edit is undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    /// Byte offset.
    pub offset: usize,
    /// Side it sticks to on an insertion at `offset`.
    pub bias: Bias,
}

impl Anchor {
    /// An anchor at `offset` with the given bias.
    pub fn new(offset: usize, bias: Bias) -> Self {
        Self { offset, bias }
    }

    /// An anchor that stays before text inserted at its offset.
    pub fn left(offset: usize) -> Self {
        Self::new(offset, Bias::Left)
    }

    /// An anchor that moves after text inserted at its offset.
    pub fn right(offset: usize) -> Self {
        Self::new(offset, Bias::Right)
    }

    /// This anchor after `transaction` is applied.
    pub fn map(self, transaction: &Transaction) -> Self {
        transaction.map_anchor(self)
    }
}

/// Handle of an anchor tracked by a [`Buffer`](crate::Buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorId(pub(crate) u64);
