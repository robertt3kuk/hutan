//! The rope-native [`VimHost`] (slice E-8): Vim's reads and edits straight on the file's
//! `hutan_buffer::Buffer`. Every read is `O(log n)` on the rope (a slice costs what it returns),
//! so a Normal command on a 1 MB file never copies the file; every Vim transaction becomes one
//! `hutan_buffer::Transaction` with the caret before and after it, so undo puts the caret back.
//!
//! The host owns the text model for the length of one key: the view lends it from its
//! [`FileBuffer`](crate::FileBuffer) ([`FileBuffer::lend`](crate::FileBuffer)), hands the host to
//! [`hutan_vim::Vim::key`], and gives the model back together with what the key did
//! ([`HostStep`]s), which the file buffer then announces to the parser and the other views like
//! any edit. The host keeps no selection of its own: the view draws a Visual selection from the
//! Vim state.

use std::borrow::Cow;
use std::ops::Range;

use hutan_buffer::{Applied, Buffer, Edit, Selections, Text, Transaction};
use hutan_vim::VimHost;

use crate::buffer::EditKind;
use crate::wrap::{WrapParams, wrap_line};

/// One change a key made to the text, in order: the text before it and what was applied.
#[derive(Debug, Clone)]
pub struct HostStep {
    pub before: Text,
    pub applied: Applied,
    pub kind: EditKind,
}

/// A [`VimHost`] over a `hutan_buffer::Buffer` it owns while one key runs.
#[derive(Debug)]
pub struct BufferHost {
    buffer: Buffer,
    cursor: usize,
    editable: bool,
    /// The Vim undo group is open (Vim's groups do not nest; the buffer's do).
    group_open: bool,
    /// The column `↑`/`↓` keep in Insert mode, in characters.
    goal: Option<usize>,
    steps: Vec<HostStep>,
    /// The lines in view (first, how many), when the view has been drawn.
    view: Option<(usize, usize)>,
    /// Where a scroll command asked the window's top to go.
    scrolled: Option<usize>,
    /// The row width the view wraps its lines to (`None`: it does not wrap).
    wrap: Option<WrapParams>,
}

impl BufferHost {
    /// A host over `buffer` with the caret at `cursor` (moved onto a character boundary).
    /// `editable`: whether edits are allowed (a read-only file still moves). `group_open`: the
    /// Vim group the previous key left open.
    pub fn new(buffer: Buffer, cursor: usize, editable: bool, group_open: bool) -> Self {
        let cursor = buffer.text().floor_char_boundary(cursor);
        Self {
            buffer,
            cursor,
            editable,
            group_open,
            goal: None,
            steps: Vec::new(),
            view: None,
            scrolled: None,
            wrap: None,
        }
    }

    pub fn text(&self) -> &Text {
        self.buffer.text()
    }

    /// Whether the Vim undo group is open.
    pub fn group_open(&self) -> bool {
        self.group_open
    }

    /// The lines the view shows (first, how many): Vim's window for `⌃D` and the like.
    pub fn set_view(&mut self, top: usize, height: usize) {
        self.view = Some((top, height));
    }

    /// Wraps lines the way the view does, for `gj` `gk` `g0` `g^` `g$`.
    pub fn set_wrap(&mut self, params: WrapParams) {
        self.wrap = Some(params);
    }

    /// The top line a scroll command asked for.
    pub fn scrolled(&self) -> Option<usize> {
        self.scrolled
    }

    /// The text model and what was done to it.
    pub fn finish(self) -> (Buffer, usize, bool, Vec<HostStep>) {
        (self.buffer, self.cursor, self.group_open, self.steps)
    }

    fn step(&mut self, before: Text, applied: Applied, kind: EditKind) {
        self.steps.push(HostStep {
            before,
            applied,
            kind,
        });
    }

    /// Applies `edits` as one transaction and leaves the caret at `cursor` (in the text after
    /// them).
    fn commit(&mut self, edits: Vec<Edit>, cursor: usize) {
        if !self.editable {
            return;
        }
        let Ok(transaction) = Transaction::new(edits) else {
            tracing::warn!("vim: an edit that does not fit the text was dropped");
            return;
        };
        if transaction.is_empty() {
            self.set_cursor(cursor);
            return;
        }
        let transaction = transaction.with_selections(
            Some(Selections::cursor(self.cursor)),
            Some(Selections::cursor(cursor)),
        );
        let before = self.buffer.snapshot();
        match self.buffer.apply(transaction) {
            Ok(applied) => {
                self.step(before, applied, EditKind::Edit);
                self.cursor = self.text().floor_char_boundary(cursor);
            }
            Err(error) => tracing::warn!("vim: edit refused: {error}"),
        }
        self.goal = None;
    }

    /// After an undo or redo: the caret the history recorded.
    fn after_history(&mut self, before: Text, applied: Applied, kind: EditKind) {
        let text = self.buffer.text();
        self.cursor = match applied.transaction.selection_after() {
            Some(selections) => selections.clamp(text).primary().head,
            None => applied
                .transaction
                .map_offset(self.cursor, hutan_buffer::Bias::Left),
        };
        self.cursor = self.text().floor_char_boundary(self.cursor);
        self.goal = None;
        self.step(before, applied, kind);
    }

    fn line_text(&self, line: usize) -> Range<usize> {
        self.text()
            .line_content_range(line)
            .unwrap_or(self.cursor..self.cursor)
    }

    /// Moves the caret `delta` lines keeping its column (in characters).
    fn move_lines(&mut self, up: bool) -> bool {
        let text = self.buffer.text();
        let Ok(line) = text.line_of_byte(self.cursor) else {
            return false;
        };
        let target = if up {
            match line.checked_sub(1) {
                Some(l) => l,
                None => return false,
            }
        } else if line + 1 < text.line_count() {
            line + 1
        } else {
            return false;
        };
        let start = text.line_start(line).unwrap_or(0);
        let column = match self.goal {
            Some(c) => c,
            None => text
                .slice(start..self.cursor)
                .unwrap_or_default()
                .chars()
                .count(),
        };
        let content = self.line_text(target);
        let line_text = self.text().slice(content.clone()).unwrap_or_default();
        let byte = line_text
            .char_indices()
            .nth(column)
            .map_or(line_text.len(), |(at, _)| at);
        self.cursor = content.start + byte;
        self.goal = Some(column);
        true
    }
}

impl VimHost for BufferHost {
    fn len(&self) -> usize {
        self.text().len()
    }

    fn char_at(&self, at: usize) -> Option<char> {
        self.text().char_at(at)
    }

    fn char_before(&self, at: usize) -> Option<char> {
        self.text().char_before(at)
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        Cow::Owned(self.text().slice(range).unwrap_or_default())
    }

    fn line_count(&self) -> usize {
        self.text().line_count()
    }

    fn line_of(&self, at: usize) -> usize {
        let text = self.text();
        text.line_of_byte(text.floor_char_boundary(at)).unwrap_or(0)
    }

    fn line_start(&self, line: usize) -> usize {
        self.text().line_start(line).unwrap_or_else(|_| self.len())
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn set_cursor(&mut self, at: usize) {
        self.cursor = self.text().floor_char_boundary(at);
        self.goal = None;
    }

    fn apply(&mut self, tx: hutan_vim::Transaction) {
        let edits = tx
            .edits
            .into_iter()
            .map(|e| Edit::new(e.range, e.text))
            .collect();
        self.commit(edits, tx.cursor);
    }

    fn type_text(&mut self, text: &str) {
        let at = self.cursor;
        self.commit(vec![Edit::insert(at, text)], at + text.len());
    }

    fn backspace(&mut self) {
        let at = self.cursor;
        if let Some(c) = self.char_before(at) {
            let from = at - c.len_utf8();
            self.commit(vec![Edit::delete(from..at)], from);
        }
    }

    fn delete_forward(&mut self) {
        let at = self.cursor;
        if let Some(c) = self.char_at(at) {
            self.commit(vec![Edit::delete(at..at + c.len_utf8())], at);
        }
    }

    fn move_left(&mut self) {
        if let Some(c) = self.char_before(self.cursor) {
            self.cursor -= c.len_utf8();
        }
        self.goal = None;
    }

    fn move_right(&mut self) {
        if let Some(c) = self.char_at(self.cursor) {
            self.cursor += c.len_utf8();
        }
        self.goal = None;
    }

    fn move_line_start(&mut self) {
        self.cursor = self.line_start_at(self.cursor);
        self.goal = None;
    }

    fn move_line_end(&mut self) {
        self.cursor = self.line_end_at(self.cursor);
        self.goal = None;
    }

    fn move_up(&mut self) -> bool {
        self.move_lines(true)
    }

    fn move_down(&mut self) -> bool {
        self.move_lines(false)
    }

    fn display_row(&self, at: usize) -> Option<Range<usize>> {
        let params = self.wrap?;
        let text = self.text();
        let at = text.floor_char_boundary(at.min(text.len()));
        let content = text.line_content_range(text.line_of_byte(at).ok()?).ok()?;
        let line = text.slice(content.clone()).ok()?;
        let breaks = wrap_line(&line, params);
        let rel = at.saturating_sub(content.start);
        let row = breaks.partition_point(|b| b.byte <= rel);
        let start = row.checked_sub(1).map_or(0, |i| breaks[i].byte);
        let end = breaks.get(row).map_or(line.len(), |b| b.byte);
        Some(content.start + start..content.start + end)
    }

    fn view(&self) -> Option<(usize, usize)> {
        self.view
            .map(|(top, height)| (self.scrolled.unwrap_or(top), height))
    }

    fn scroll_to(&mut self, top: usize) {
        self.scrolled = Some(top);
    }

    fn begin_group(&mut self) {
        if !self.group_open {
            self.buffer.begin_group();
            self.group_open = true;
        }
    }

    fn end_group(&mut self) {
        if self.group_open {
            self.buffer.end_group();
            self.group_open = false;
        }
    }

    fn undo(&mut self) -> bool {
        if !self.editable {
            return false;
        }
        // `Buffer::undo` closes every open group itself.
        self.group_open = false;
        let before = self.buffer.snapshot();
        match self.buffer.undo() {
            Some(applied) => {
                self.after_history(before, applied, EditKind::Undo);
                true
            }
            None => false,
        }
    }

    fn redo(&mut self) -> bool {
        if !self.editable {
            return false;
        }
        self.group_open = false;
        let before = self.buffer.snapshot();
        match self.buffer.redo() {
            Some(applied) => {
                self.after_history(before, applied, EditKind::Redo);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_vim::{Key, Mode, Options, Vim};

    fn host(text: &str) -> BufferHost {
        BufferHost::new(Buffer::from_content(text), 0, true, false)
    }

    fn keys(vim: &mut Vim<BufferHost>, host: &mut BufferHost, keys: &str) {
        let opts = Options {
            insert_passthrough: false,
            ..Options::default()
        };
        for c in keys.chars() {
            let key = if c == '\u{1b}' {
                Key::Escape
            } else {
                Key::Char(c)
            };
            vim.key(host, key, opts, &mut ());
        }
    }

    #[test]
    fn a_command_is_one_transaction_and_one_undo_step() {
        let mut host = host("alpha beta\ngamma\n");
        let mut vim = Vim::new();
        keys(&mut vim, &mut host, "\u{1b}");
        assert_eq!(vim.mode(), Mode::Normal);
        keys(&mut vim, &mut host, "wdw");
        assert_eq!(host.text().to_string(), "alpha \ngamma\n");
        keys(&mut vim, &mut host, "jdd");
        assert_eq!(host.text().to_string(), "alpha \n");
        keys(&mut vim, &mut host, "u");
        assert_eq!(host.text().to_string(), "alpha \ngamma\n");
        keys(&mut vim, &mut host, "u");
        assert_eq!(host.text().to_string(), "alpha beta\ngamma\n");
        assert_eq!(
            host.cursor(),
            6,
            "undo puts the caret back where the change was"
        );
        let (_, _, open, steps) = host.finish();
        assert!(!open);
        let kinds: Vec<EditKind> = steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                EditKind::Edit,
                EditKind::Edit,
                EditKind::Undo,
                EditKind::Undo
            ]
        );
    }

    #[test]
    fn an_insert_session_is_one_undo_step() {
        let mut host = host("one\n");
        let mut vim = Vim::new();
        keys(&mut vim, &mut host, "\u{1b}A two three\u{1b}");
        assert_eq!(host.text().to_string(), "one two three\n");
        keys(&mut vim, &mut host, "u");
        assert_eq!(host.text().to_string(), "one\n");
    }

    #[test]
    fn a_read_only_text_moves_but_never_changes() {
        let mut host = BufferHost::new(Buffer::from_content("abc def\n"), 0, false, false);
        let mut vim = Vim::new();
        keys(&mut vim, &mut host, "\u{1b}wx");
        assert_eq!(host.cursor(), 4);
        assert_eq!(host.text().to_string(), "abc def\n");
        assert!(host.finish().3.is_empty());
    }
}
