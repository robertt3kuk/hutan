//! What the modal layer needs from the text it edits.

use std::borrow::Cow;
use std::ops::Range;

/// Replace the bytes of `range` with `text` (an insertion has an empty range, a deletion empty
/// text). `range` is in the coordinates of the text before the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

/// An atomic set of edits and where the caret rests afterwards. The edits are sorted, do not
/// overlap (touching is fine) and apply simultaneously, all or none; `cursor` is a byte offset in
/// the text *after* them. A host over `hutan_buffer::Buffer` maps this one to one onto its
/// `Transaction` and puts the selection at `cursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub edits: Vec<Edit>,
    pub cursor: usize,
}

impl Transaction {
    /// One edit: `range` becomes `text`.
    pub fn replace(range: Range<usize>, text: impl Into<String>, cursor: usize) -> Self {
        Self {
            edits: vec![Edit {
                range,
                text: text.into(),
            }],
            cursor,
        }
    }

    /// Removes `range`.
    pub fn delete(range: Range<usize>, cursor: usize) -> Self {
        Self::replace(range, "", cursor)
    }

    /// Inserts `text` at `at`.
    pub fn insert(at: usize, text: impl Into<String>, cursor: usize) -> Self {
        Self::replace(at..at, text, cursor)
    }
}

/// The text a [`Vim`](crate::Vim) edits, seen through byte offsets.
///
/// Every offset is a byte offset on a character boundary of the host's current text; a line ends
/// at a `\n` (which belongs to no line's text) or at the end of the text, and a text that ends in
/// `\n` has an empty last line here. Vim does not see that one: a final `\n` is the file's final
/// newline (`'eol'`), so motions stop on the line before it and edits leave it in place. The
/// reads are what motions, text objects and operators need, so
/// that a rope can answer each in `O(log n)` without flattening the text (`slice` in the length
/// of what it returns, which is a word, a line or an operator's span, never the whole text). The
/// edits are the only way the modal layer changes the text: [`apply`](Self::apply) for what Vim
/// itself computes, and the `type_text` to `move_down` group for the keys Insert mode leaves to
/// the host's own editing (selection aware, the way every other text of the application edits).
pub trait VimHost {
    // ---- reads -------------------------------------------------------------------------------

    /// The length of the text in bytes.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The character that starts at byte `at`; `None` at the end of the text and when `at` is not
    /// a character boundary.
    fn char_at(&self, at: usize) -> Option<char>;

    /// The character that ends at byte `at`; `None` at the start of the text and when `at` is not
    /// a character boundary.
    fn char_before(&self, at: usize) -> Option<char>;

    /// The bytes `range` (character boundaries inside the text).
    fn slice(&self, range: Range<usize>) -> Cow<'_, str>;

    /// How many lines the text has (one more than its number of `\n`).
    fn line_count(&self) -> usize;

    /// The line that holds byte `at`: the number of `\n` before it.
    fn line_of(&self, at: usize) -> usize;

    /// The offset where line `line` starts (`line < line_count()`).
    fn line_start(&self, line: usize) -> usize;

    /// The offset where line `line` ends: its `\n`, or the end of the text.
    fn line_end(&self, line: usize) -> usize {
        if line + 1 < self.line_count() {
            self.line_start(line + 1) - 1
        } else {
            self.len()
        }
    }

    /// The start of the line that holds `at`.
    fn line_start_at(&self, at: usize) -> usize {
        self.line_start(self.line_of(at))
    }

    /// The end of the line that holds `at`.
    fn line_end_at(&self, at: usize) -> usize {
        self.line_end(self.line_of(at))
    }

    /// The caret.
    fn cursor(&self) -> usize;

    // ---- caret and edits ---------------------------------------------------------------------

    /// Puts the caret at `at` (or at the nearest character boundary before it) and drops any
    /// selection the host keeps.
    fn set_cursor(&mut self, at: usize);

    /// Applies the edits atomically and puts the caret at `tx.cursor`. Within a group
    /// ([`begin_group`](Self::begin_group)) they join its undo step.
    fn apply(&mut self, tx: Transaction);

    /// Types `text` at the caret, replacing the selection when the host has one (Insert mode).
    fn type_text(&mut self, text: &str);

    /// Backspace: the selection, else the character before the caret.
    fn backspace(&mut self);

    /// Delete: the selection, else the character after the caret.
    fn delete_forward(&mut self);

    /// ←: one character back; with a selection, its start.
    fn move_left(&mut self);

    /// →: one character on; with a selection, its end.
    fn move_right(&mut self);

    /// Home: the start of the caret's line.
    fn move_line_start(&mut self);

    /// End: the end of the caret's line.
    fn move_line_end(&mut self);

    /// ↑: one line up at the same column. `false` when the caret did not move.
    fn move_up(&mut self) -> bool;

    /// ↓: one line down at the same column. `false` when the caret did not move.
    fn move_down(&mut self) -> bool;

    // ---- the window --------------------------------------------------------------------------

    /// The lines in view: the first (0-based) and how many fit. `None` (the default) for a host
    /// that shows no window; `⌃D` `⌃U` `⌃F` `⌃B` `⌃E` `⌃Y` do nothing there.
    fn view(&self) -> Option<(usize, usize)> {
        None
    }

    /// Scrolls so that line `top` is the first in view.
    fn scroll_to(&mut self, _top: usize) {}

    // ---- undo --------------------------------------------------------------------------------

    /// Starts a group: every edit until [`end_group`](Self::end_group) undoes in one step. A
    /// group already open stays the one group.
    fn begin_group(&mut self);

    /// Ends the group; a group that changed nothing leaves no step behind.
    fn end_group(&mut self);

    /// Undoes one step; `false` when there is none.
    fn undo(&mut self) -> bool;

    /// Redoes one undone step; `false` when there is none.
    fn redo(&mut self) -> bool;
}

/// The system clipboard, as far as the register needs it.
pub trait Clipboard {
    fn read(&mut self) -> Option<String>;
    fn write(&mut self, text: &str);
}

/// No clipboard (the register is private to the field).
impl Clipboard for () {
    fn read(&mut self) -> Option<String> {
        None
    }
    fn write(&mut self, _: &str) {}
}
