//! A reference host (a `String` with snapshot undo) and a rig that feeds it keys written like
//! Vim documents them.

use std::borrow::Cow;
use std::ops::Range;

use hutan_vim::{Clipboard, Key, Mode, Options, Outcome, Transaction, Vim, VimHost};

/// A text, a caret and an undo stack. Every call checks the contract the modal layer promises
/// its host: offsets on character boundaries, transactions sorted and not overlapping.
#[derive(Debug, Default)]
pub struct StringHost {
    text: String,
    cursor: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    grouped: bool,
    /// Every transaction applied, in order.
    pub log: Vec<Transaction>,
    /// How many groups were opened and closed.
    pub begun: usize,
    pub ended: usize,
    /// How many bytes `slice` handed out.
    pub sliced: std::cell::Cell<usize>,
}

impl StringHost {
    pub fn with(text: &str, cursor: usize) -> Self {
        Self {
            text: text.to_owned(),
            cursor,
            ..Self::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_grouped(&self) -> bool {
        self.grouped
    }

    fn assert_boundary(&self, at: usize) {
        assert!(
            at <= self.text.len() && self.text.is_char_boundary(at),
            "offset {at} is not a boundary of {:?}",
            self.text
        );
    }

    fn step(&mut self) {
        if !self.grouped {
            self.undo.push((self.text.clone(), self.cursor));
            self.redo.clear();
        }
    }

    fn line_start_of(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end_of(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    fn column(&self, at: usize) -> usize {
        self.text[self.line_start_of(at)..at].chars().count()
    }

    fn at_column(&self, start: usize, end: usize, column: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i)
    }
}

impl VimHost for StringHost {
    fn len(&self) -> usize {
        self.text.len()
    }

    fn char_at(&self, at: usize) -> Option<char> {
        self.text.get(at..)?.chars().next()
    }

    fn char_before(&self, at: usize) -> Option<char> {
        self.text.get(..at)?.chars().next_back()
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        self.assert_boundary(range.start);
        self.assert_boundary(range.end);
        assert!(range.start <= range.end, "reversed range {range:?}");
        self.sliced.set(self.sliced.get() + range.len());
        Cow::Borrowed(&self.text[range])
    }

    fn line_count(&self) -> usize {
        self.text.matches('\n').count() + 1
    }

    fn line_of(&self, at: usize) -> usize {
        self.assert_boundary(at);
        self.text[..at].matches('\n').count()
    }

    fn line_start(&self, line: usize) -> usize {
        assert!(line < self.line_count(), "no line {line}");
        if line == 0 {
            return 0;
        }
        self.text
            .match_indices('\n')
            .nth(line - 1)
            .map_or(self.text.len(), |(i, _)| i + 1)
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn set_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.text.len());
        while !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    fn apply(&mut self, tx: Transaction) {
        let mut prev_end = 0;
        for edit in &tx.edits {
            self.assert_boundary(edit.range.start);
            self.assert_boundary(edit.range.end);
            assert!(edit.range.start <= edit.range.end, "reversed edit");
            assert!(
                edit.range.start >= prev_end,
                "edits overlap or are unsorted"
            );
            prev_end = edit.range.end;
        }
        self.step();
        for edit in tx.edits.iter().rev() {
            self.text.replace_range(edit.range.clone(), &edit.text);
        }
        self.assert_boundary(tx.cursor);
        self.cursor = tx.cursor;
        self.log.push(tx);
    }

    fn type_text(&mut self, text: &str) {
        self.step();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn backspace(&mut self) {
        if let Some(prev) = self.text[..self.cursor].char_indices().next_back() {
            self.step();
            self.text.drain(prev.0..self.cursor);
            self.cursor = prev.0;
        }
    }

    fn delete_forward(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.step();
            self.text.drain(self.cursor..self.cursor + c.len_utf8());
        }
    }

    fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
    }

    fn move_right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    fn move_line_start(&mut self) {
        self.cursor = self.line_start_of(self.cursor);
    }

    fn move_line_end(&mut self) {
        self.cursor = self.line_end_of(self.cursor);
    }

    fn move_up(&mut self) -> bool {
        let start = self.line_start_of(self.cursor);
        if start == 0 {
            return false;
        }
        let column = self.column(self.cursor);
        let prev_end = start - 1;
        self.cursor = self.at_column(self.line_start_of(prev_end), prev_end, column);
        true
    }

    fn move_down(&mut self) -> bool {
        let end = self.line_end_of(self.cursor);
        if end == self.text.len() {
            return false;
        }
        let column = self.column(self.cursor);
        self.cursor = self.at_column(end + 1, self.line_end_of(end + 1), column);
        true
    }

    fn begin_group(&mut self) {
        if !self.grouped {
            self.undo.push((self.text.clone(), self.cursor));
            self.redo.clear();
            self.grouped = true;
            self.begun += 1;
        }
    }

    fn end_group(&mut self) {
        if self.grouped {
            self.grouped = false;
            self.ended += 1;
            if self.undo.last().is_some_and(|(text, _)| *text == self.text) {
                self.undo.pop();
            }
        }
    }

    fn undo(&mut self) -> bool {
        self.grouped = false;
        let Some((text, cursor)) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push((std::mem::replace(&mut self.text, text), self.cursor));
        self.cursor = cursor;
        true
    }

    fn redo(&mut self) -> bool {
        self.grouped = false;
        let Some((text, cursor)) = self.redo.pop() else {
            return false;
        };
        self.undo
            .push((std::mem::replace(&mut self.text, text), self.cursor));
        self.cursor = cursor;
        true
    }
}

/// An in-memory system clipboard.
#[derive(Debug, Default)]
pub struct Board(pub Option<String>);

impl Clipboard for Board {
    fn read(&mut self) -> Option<String> {
        self.0.clone()
    }

    fn write(&mut self, text: &str) {
        self.0 = Some(text.to_owned());
    }
}

/// A field and its modal state, in Normal mode with the caret at `|` in the text (at the end
/// when there is none).
#[derive(Debug)]
pub struct Rig {
    pub host: StringHost,
    pub vim: Vim<StringHost>,
    pub opts: Options,
    pub clip: Board,
}

impl Rig {
    pub fn new(text: &str) -> Self {
        let (plain, cursor) = match text.find('|') {
            Some(at) => (text.replacen('|', "", 1), at),
            None => (text.to_owned(), text.len()),
        };
        let mut host = StringHost::default();
        let mut vim = Vim::new();
        // A fresh `Vim` types; Escape is how it gets to Normal mode.
        vim.key(&mut host, Key::Escape, Options::default(), &mut ());
        host = StringHost::with(&plain, cursor);
        Self {
            host,
            vim,
            opts: Options::default(),
            clip: Board::default(),
        }
    }

    pub fn single(text: &str) -> Self {
        let mut rig = Self::new(text);
        rig.opts.single_line = true;
        rig
    }

    pub fn key(&mut self, key: Key) -> Outcome {
        self.vim.key(&mut self.host, key, self.opts, &mut self.clip)
    }

    /// Types plain characters and `<Esc>`, `<CR>`, `<BS>`, `<Left>`, `<C-r>`.
    pub fn keys(&mut self, keys: &str) -> &mut Self {
        for key in parse_keys(keys) {
            self.key(key);
        }
        self
    }

    pub fn text(&self) -> &str {
        self.host.text()
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// The text with `|` before the caret.
    pub fn shown(&self) -> String {
        let mut s = self.host.text().to_owned();
        s.insert(self.host.cursor(), '|');
        s
    }
}

/// `text` in Normal mode, `keys` typed, the result shown with the caret.
pub fn run(text: &str, keys: &str) -> String {
    Rig::new(text).keys(keys).shown()
}

/// Keys written like Vim documents them: plain characters, `<Esc>`, `<CR>`, `<BS>`, `<Left>`,
/// `<C-r>`.
pub fn parse_keys(keys: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut chars = keys.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '<' {
            let name: String = chars.by_ref().take_while(|c| *c != '>').collect();
            match name.as_str() {
                "Esc" => Key::Escape,
                "CR" => Key::Enter,
                "BS" => Key::Backspace,
                "Left" => Key::Left,
                n if n.starts_with("C-") => Key::Ctrl(n.chars().nth(2).unwrap()),
                n => panic!("unknown key <{n}>"),
            }
        } else {
            Key::Char(c)
        });
    }
    out
}
