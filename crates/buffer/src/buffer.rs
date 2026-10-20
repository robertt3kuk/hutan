use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::time::Instant;

use crate::anchor::{Anchor, AnchorId};
use crate::diff;
use crate::edit::Edit;
use crate::error::BufferError;
use crate::history::{History, HistoryLimits};
use crate::text::Text;
use crate::transaction::Transaction;

/// Most anchors one [`Buffer`] tracks.
pub const MAX_ANCHORS: usize = 100_000;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// The line ending a file is written with. In memory the text always uses `\n`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\n`
    #[default]
    Lf,
    /// `\r\n`
    CrLf,
}

impl LineEnding {
    /// The terminator as a string.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// What a change to the buffer did: the transaction that was applied to the text (for an undo,
/// the inverse of the group that was undone) and the buffer version after it.
///
/// Feed [`transaction`](Self::transaction) to everything that mirrors the text (parser, LSP
/// document, other views' selections and anchors via [`Transaction::map_offset`]); its
/// [`selection_after`](Transaction::selection_after) is the selection to restore, when one was
/// recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The transaction applied to the text.
    pub transaction: Transaction,
    /// The buffer's version after applying it (unchanged for an empty transaction).
    pub version: u64,
}

/// An editable text: a rope plus how it is stored on disk, an undo tree, tracked anchors and a
/// version counter.
///
/// The text is kept with `\n` line breaks and without a BOM whatever the file used; the
/// original line ending ([`LineEnding`]) and BOM are remembered and written back by
/// [`write_to`](Self::write_to), so a CRLF file stays CRLF. A file that mixes both is normalized to
/// its majority ending ([`has_mixed_line_endings`](Self::has_mixed_line_endings) tells the UI).
/// Edits are not normalized: text inserted with `\r\n` stays as given (use
/// [`normalize_newlines`]).
///
/// ## Undo
/// Every [`apply`](Self::apply) is one undoable *group*, unless a group is open
/// ([`begin_group`](Self::begin_group)): then all transactions until the matching
/// [`end_group`](Self::end_group) are composed into one group, so one Vim command or one insert
/// session is one undo step. Undoing and then editing starts a new branch; the old branch stays
/// reachable with [`redo_branch`](Self::redo_branch). See [`HistoryLimits`] for the bounds.
pub struct Buffer {
    text: Text,
    version: u64,
    line_ending: LineEnding,
    mixed_line_endings: bool,
    bom: bool,
    saved_line_ending: LineEnding,
    saved_bom: bool,
    history: History,
    anchors: HashMap<AnchorId, Anchor>,
    next_anchor: u64,
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("text", &self.text)
            .field("version", &self.version)
            .field("line_ending", &self.line_ending)
            .field("bom", &self.bom)
            .field("dirty", &self.is_dirty())
            .finish()
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Text as the buffer stores it, and how it was stored.
struct Prepared<'a> {
    bom: bool,
    /// `None` when the text has no line break.
    line_ending: Option<LineEnding>,
    mixed: bool,
    text: Cow<'a, str>,
}

fn prepare(s: &str) -> Prepared<'_> {
    let (bom, s) = match s.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let bytes = s.as_bytes();
    let (mut lf, mut crlf) = (0usize, 0usize);
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    let line_ending = match (lf, crlf) {
        (0, 0) => None,
        (lf, crlf) if crlf > lf => Some(LineEnding::CrLf),
        _ => Some(LineEnding::Lf),
    };
    let text = if crlf > 0 {
        Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(s)
    };
    Prepared {
        bom,
        line_ending,
        mixed: lf > 0 && crlf > 0,
        text,
    }
}

/// Replaces every `\r\n` with `\n`: what to run over text arriving from a clipboard or a file
/// before inserting it.
pub fn normalize_newlines(s: &str) -> Cow<'_, str> {
    if s.contains("\r\n") {
        Cow::Owned(s.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Checks that bytes are text: no NUL (binary) and valid UTF-8 (a BOM is kept; `prepare` records it).
fn decode(bytes: &[u8]) -> Result<&str, BufferError> {
    if let Some(offset) = bytes.iter().position(|b| *b == 0) {
        return Err(BufferError::Binary { offset });
    }
    std::str::from_utf8(bytes).map_err(|e| BufferError::InvalidUtf8 {
        valid_up_to: e.valid_up_to(),
    })
}

impl Buffer {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::from_text(Text::new(), LineEnding::Lf, false, false)
    }

    fn from_text(text: Text, line_ending: LineEnding, mixed: bool, bom: bool) -> Self {
        Self {
            text,
            version: 0,
            line_ending,
            mixed_line_endings: mixed,
            bom,
            saved_line_ending: line_ending,
            saved_bom: bom,
            history: History::new(Instant::now()),
            anchors: HashMap::new(),
            next_anchor: 0,
        }
    }

    /// A buffer holding `s` (a leading BOM and CRLF line endings are detected and normalized as
    /// for a file).
    pub fn from_content(s: &str) -> Self {
        let p = prepare(s);
        Self::from_text(
            Text::from(&*p.text),
            p.line_ending.unwrap_or_default(),
            p.mixed,
            p.bom,
        )
    }

    /// A buffer from the bytes of a file: refuses binary content (a NUL byte,
    /// [`BufferError::Binary`]) and anything that is not UTF-8 ([`BufferError::InvalidUtf8`]);
    /// detects and normalizes the BOM and line endings.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BufferError> {
        Ok(Self::from_content(decode(bytes)?))
    }

    /// Like [`from_bytes`](Self::from_bytes), reading at most `max_bytes` from `reader`
    /// ([`BufferError::TooLarge`] when there is more).
    pub fn from_reader(reader: impl Read, max_bytes: usize) -> Result<Self, BufferError> {
        let mut bytes = Vec::new();
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        reader.take(limit).read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(BufferError::TooLarge { limit: max_bytes });
        }
        Self::from_bytes(&bytes)
    }

    // ---- state -----------------------------------------------------------------------------

    /// The current text.
    pub fn text(&self) -> &Text {
        &self.text
    }

    /// An `O(1)` immutable copy of the current text, unaffected by later edits.
    pub fn snapshot(&self) -> Text {
        self.text.clone()
    }

    /// Counts every change of the text (edits, undo, redo, external changes); use it to tag
    /// snapshots and detect stale background results.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The line ending the buffer is written with.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the loaded file mixed `\n` and `\r\n` (it is written with one of them now).
    pub fn has_mixed_line_endings(&self) -> bool {
        self.mixed_line_endings
    }

    /// Changes the line ending the buffer is written with; the buffer is dirty until saved.
    pub fn set_line_ending(&mut self, ending: LineEnding) {
        self.line_ending = ending;
        self.mixed_line_endings = false;
    }

    /// Whether the file is written with a UTF-8 BOM.
    pub fn has_bom(&self) -> bool {
        self.bom
    }

    /// Sets whether a UTF-8 BOM is written; the buffer is dirty until saved.
    pub fn set_bom(&mut self, bom: bool) {
        self.bom = bom;
    }

    /// Whether the buffer differs from what was last saved (or loaded): the undo history is not at
    /// the saved state, or the line ending or BOM changed.
    pub fn is_dirty(&self) -> bool {
        !self.history.is_clean()
            || self.line_ending != self.saved_line_ending
            || self.bom != self.saved_bom
    }

    /// Records the current state as the saved one (call it after the bytes of
    /// [`write_to`](Self::write_to) reached the disk, or after a clean buffer was reloaded with
    /// [`apply_external`](Self::apply_external)).
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
        self.saved_line_ending = self.line_ending;
        self.saved_bom = self.bom;
    }

    /// Writes the file bytes: BOM, then the text with the buffer's line ending.
    pub fn write_to(&self, mut writer: impl Write) -> io::Result<()> {
        if self.bom {
            writer.write_all(UTF8_BOM)?;
        }
        for chunk in self.text.chunks() {
            match self.line_ending {
                LineEnding::Lf => writer.write_all(chunk.as_bytes())?,
                LineEnding::CrLf => {
                    let mut rest = chunk;
                    while let Some(i) = rest.find('\n') {
                        writer.write_all(&rest.as_bytes()[..i])?;
                        writer.write_all(b"\r\n")?;
                        rest = &rest[i + 1..];
                    }
                    writer.write_all(rest.as_bytes())?;
                }
            }
        }
        Ok(())
    }

    /// The file bytes ([`write_to`](Self::write_to) into a `Vec`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len() + 3);
        // Writing to a Vec cannot fail.
        let _ = self.write_to(&mut out);
        out
    }

    // ---- editing ---------------------------------------------------------------------------

    /// Applies a transaction atomically: all of it or, when it is invalid for the current text
    /// (out of bounds, inside a character), none. Recorded as one undo group (or joined to the
    /// open one). Tracked anchors move with it.
    pub fn apply(&mut self, transaction: Transaction) -> Result<Applied, BufferError> {
        self.apply_inner(transaction, false)
    }

    /// Replaces `range` with `text` (one transaction).
    pub fn replace(
        &mut self,
        range: Range<usize>,
        text: impl Into<std::sync::Arc<str>>,
    ) -> Result<Applied, BufferError> {
        self.apply(Transaction::replace(range, text)?)
    }

    /// Inserts `text` at `offset` (one transaction).
    pub fn insert(
        &mut self,
        offset: usize,
        text: impl Into<std::sync::Arc<str>>,
    ) -> Result<Applied, BufferError> {
        self.apply(Transaction::insert(offset, text))
    }

    /// Deletes `range` (one transaction).
    pub fn delete(&mut self, range: Range<usize>) -> Result<Applied, BufferError> {
        self.apply(Transaction::delete(range)?)
    }

    /// Applies several edits (coordinates of the current text) as one transaction.
    pub fn edit(&mut self, edits: impl IntoIterator<Item = Edit>) -> Result<Applied, BufferError> {
        self.apply(Transaction::new(edits)?)
    }

    fn apply_inner(
        &mut self,
        transaction: Transaction,
        new_group: bool,
    ) -> Result<Applied, BufferError> {
        if transaction.is_empty() {
            return Ok(Applied {
                transaction,
                version: self.version,
            });
        }
        // `invert` validates the edits against the current text, so nothing below can fail halfway.
        let inverse = transaction.invert(&self.text)?;
        if new_group {
            self.history.split_group();
        }
        self.history
            .record(transaction.clone(), inverse, Instant::now())?;
        if new_group {
            // Later edits of the same open group start a group of their own.
            self.history.split_group();
        }
        self.text.apply_validated(transaction.edits());
        self.version += 1;
        self.map_anchors(&transaction);
        Ok(Applied {
            transaction,
            version: self.version,
        })
    }

    fn map_anchors(&mut self, transaction: &Transaction) {
        for anchor in self.anchors.values_mut() {
            *anchor = transaction.map_anchor(*anchor);
        }
    }

    /// Makes the buffer's text equal to `new_text` (content that arrived from outside, such as an
    /// agent's edit on disk) by applying the minimal difference as **one** transaction and one
    /// undo step, so anchors and selections in unchanged regions stay where they are and undo
    /// steps back over the change. It never joins an open group.
    ///
    /// `new_text` is treated like file content: a leading BOM and CRLF line endings are detected
    /// (the buffer's [`line_ending`](Self::line_ending) and BOM follow the new content; a text
    /// without line breaks keeps the current ending) and normalized. Returns `None` when the text
    /// already matches. The caller decides what that means for the dirty state: after reloading a
    /// *clean* buffer call [`mark_saved`](Self::mark_saved).
    pub fn apply_external(&mut self, new_text: &str) -> Result<Option<Applied>, BufferError> {
        let p = prepare(new_text);
        self.bom = p.bom;
        if let Some(ending) = p.line_ending {
            self.line_ending = ending;
        }
        self.mixed_line_endings = p.mixed;
        let old = self.text.slice(0..self.text.len())?;
        let edits = diff::minimal_edits(&old, &p.text);
        if edits.is_empty() {
            return Ok(None);
        }
        self.apply_inner(Transaction::new(edits)?, true).map(Some)
    }

    /// [`apply_external`](Self::apply_external) for the bytes of a file: binary and non-UTF-8
    /// content is refused and the buffer is left untouched.
    pub fn apply_external_bytes(&mut self, bytes: &[u8]) -> Result<Option<Applied>, BufferError> {
        self.apply_external(decode(bytes)?)
    }

    // ---- undo ------------------------------------------------------------------------------

    /// Opens an undo group: until the matching [`end_group`](Self::end_group) every applied
    /// transaction joins the same group. Groups nest (only the outermost counts).
    pub fn begin_group(&mut self) {
        self.history.begin_group();
    }

    /// Closes the group opened by [`begin_group`](Self::begin_group).
    pub fn end_group(&mut self) {
        self.history.end_group();
    }

    /// Runs `f` inside a group.
    pub fn group<R>(&mut self, f: impl FnOnce(&mut Buffer) -> R) -> R {
        self.begin_group();
        let result = f(self);
        self.end_group();
        result
    }

    /// Whether there is a group to undo.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether there is a group to redo.
    pub fn can_redo(&self) -> bool {
        self.history.redo_branches() > 0
    }

    /// Undoes the last group (closing an open one first). `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<Applied> {
        self.history.close_groups();
        let transaction = self.history.undo_transaction()?;
        self.move_in_history(transaction, History::step_back)
    }

    /// Redoes the group undone last, or created last. `None` when there is nothing to redo.
    pub fn redo(&mut self) -> Option<Applied> {
        self.redo_branch_inner(None)
    }

    /// How many groups [`redo_branch`](Self::redo_branch) can choose between at this point.
    pub fn redo_branches(&self) -> usize {
        self.history.redo_branches()
    }

    /// Redoes the `index`-th branch that starts here (0 = the oldest).
    pub fn redo_branch(&mut self, index: usize) -> Option<Applied> {
        self.redo_branch_inner(Some(index))
    }

    fn redo_branch_inner(&mut self, branch: Option<usize>) -> Option<Applied> {
        self.history.close_groups();
        let (child, transaction) = self.history.redo_transaction(branch)?;
        self.move_in_history(transaction, move |h| h.step_forward(child))
    }

    fn move_in_history(
        &mut self,
        transaction: Transaction,
        advance: impl FnOnce(&mut History),
    ) -> Option<Applied> {
        // The history is linear in the text's states, so the transaction always fits; a failure
        // here would mean the invariant is broken, and refusing to move is the safe answer.
        if self.text.validate_edits(transaction.edits()).is_err() {
            return None;
        }
        self.text.apply_validated(transaction.edits());
        advance(&mut self.history);
        if !transaction.is_empty() {
            self.version += 1;
            self.map_anchors(&transaction);
        }
        Some(Applied {
            transaction,
            version: self.version,
        })
    }

    /// The bounds of the undo history.
    pub fn history_limits(&self) -> HistoryLimits {
        self.history.limits()
    }

    /// Sets the bounds of the undo history and applies them at once.
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.history.set_limits(limits, Instant::now());
    }

    /// Applies the history bounds as of `now` (they are applied after every edit with the
    /// current time; this lets a caller age the history without editing).
    pub fn enforce_history_limits(&mut self, now: Instant) {
        self.history.prune(now);
    }

    /// Number of undo groups held (all branches).
    pub fn history_groups(&self) -> usize {
        self.history.groups()
    }

    /// Approximate bytes the undo history holds.
    pub fn history_bytes(&self) -> usize {
        self.history.bytes()
    }

    // ---- anchors ---------------------------------------------------------------------------

    /// Starts tracking an anchor: it is moved through every later edit, undo, redo and external
    /// change. At most [`MAX_ANCHORS`].
    pub fn track_anchor(&mut self, anchor: Anchor) -> Result<AnchorId, BufferError> {
        self.text.check_offset(anchor.offset)?;
        if self.anchors.len() >= MAX_ANCHORS {
            return Err(BufferError::TooManyAnchors { max: MAX_ANCHORS });
        }
        let id = AnchorId(self.next_anchor);
        self.next_anchor += 1;
        self.anchors.insert(id, anchor);
        Ok(id)
    }

    /// The current position of a tracked anchor.
    pub fn anchor(&self, id: AnchorId) -> Option<Anchor> {
        self.anchors.get(&id).copied()
    }

    /// Stops tracking an anchor, returning its last position.
    pub fn untrack_anchor(&mut self, id: AnchorId) -> Option<Anchor> {
        self.anchors.remove(&id)
    }

    /// Number of tracked anchors.
    pub fn tracked_anchors(&self) -> usize {
        self.anchors.len()
    }
}
