//! A complete, minimal [`VimHost`] (a `String`, a caret, snapshot undo) and a driver: the
//! worked example of the trait, and the binary `docs/perf.md` measures the size of the Vim layer
//! with (release, `strip = "symbols"`).
//!
//! ```text
//! cargo run -p hutan-vim --example size_probe -- 'foo |bar' 'dw'    # prints "foo| "
//! ```
//!
//! The text is the first argument (`|` marks the caret; at the end when there is none), the keys
//! the second (plain characters and `<Esc>`, `<CR>`, `<BS>`).

use std::borrow::Cow;
use std::ops::Range;

use hutan_vim::{Key, Options, Transaction, Vim, VimHost};

#[derive(Default)]
struct Host {
    text: String,
    cursor: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    grouped: bool,
}

impl Host {
    fn step(&mut self) {
        if !self.grouped {
            self.undo.push((self.text.clone(), self.cursor));
            self.redo.clear();
        }
    }

    fn line_start_at_cursor(&self) -> usize {
        self.line_start_at(self.cursor)
    }
}

impl VimHost for Host {
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
        Cow::Borrowed(&self.text[range])
    }

    fn line_count(&self) -> usize {
        self.text.matches('\n').count() + 1
    }

    fn line_of(&self, at: usize) -> usize {
        self.text[..at].matches('\n').count()
    }

    fn line_start(&self, line: usize) -> usize {
        match line.checked_sub(1) {
            None => 0,
            Some(n) => self
                .text
                .match_indices('\n')
                .nth(n)
                .map_or(self.text.len(), |(at, _)| at + 1),
        }
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
        self.step();
        // Sorted and non-overlapping, in the coordinates of the text before: back to front.
        for edit in tx.edits.iter().rev() {
            self.text.replace_range(edit.range.clone(), &edit.text);
        }
        self.cursor = tx.cursor;
    }

    fn type_text(&mut self, text: &str) {
        self.step();
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn backspace(&mut self) {
        if let Some((at, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.step();
            self.text.drain(at..self.cursor);
            self.cursor = at;
        }
    }

    fn delete_forward(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.step();
            self.text.drain(self.cursor..self.cursor + c.len_utf8());
        }
    }

    fn move_left(&mut self) {
        self.set_cursor(self.cursor.saturating_sub(1));
    }

    fn move_right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    fn move_line_start(&mut self) {
        self.cursor = self.line_start_at_cursor();
    }

    fn move_line_end(&mut self) {
        self.cursor = self.line_end_at(self.cursor);
    }

    fn move_up(&mut self) -> bool {
        let line = self.line_of(self.cursor);
        if line == 0 {
            return false;
        }
        let column = self.text[self.line_start(line)..self.cursor]
            .chars()
            .count();
        let (start, end) = (self.line_start(line - 1), self.line_end(line - 1));
        self.cursor = self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i);
        true
    }

    fn move_down(&mut self) -> bool {
        let line = self.line_of(self.cursor);
        if line + 1 >= self.line_count() {
            return false;
        }
        let column = self.text[self.line_start(line)..self.cursor]
            .chars()
            .count();
        let (start, end) = (self.line_start(line + 1), self.line_end(line + 1));
        self.cursor = self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i);
        true
    }

    fn begin_group(&mut self) {
        if !self.grouped {
            self.undo.push((self.text.clone(), self.cursor));
            self.redo.clear();
            self.grouped = true;
        }
    }

    fn end_group(&mut self) {
        self.grouped = false;
        if self.undo.last().is_some_and(|(text, _)| *text == self.text) {
            self.undo.pop();
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

fn main() {
    let mut args = std::env::args().skip(1);
    let given = args.next().unwrap_or_else(|| "foo bar".to_owned());
    let keys = args.next().unwrap_or_else(|| "wdw".to_owned());
    let (text, cursor) = match given.find('|') {
        Some(at) => (given.replacen('|', "", 1), at),
        None => (given.clone(), given.len()),
    };
    let mut host = Host {
        text,
        cursor,
        ..Host::default()
    };
    // A fresh `Vim` types; Escape is how it gets to Normal mode.
    let mut vim = Vim::new();
    let opts = Options::default();
    vim.key(&mut host, Key::Escape, opts, &mut ());
    host.cursor = cursor;
    let mut chars = keys.chars();
    while let Some(c) = chars.next() {
        let key = if c == '<' {
            match chars
                .by_ref()
                .take_while(|c| *c != '>')
                .collect::<String>()
                .as_str()
            {
                "Esc" => Key::Escape,
                "CR" => Key::Enter,
                "BS" => Key::Backspace,
                other => {
                    eprintln!("unknown key <{other}>");
                    continue;
                }
            }
        } else {
            Key::Char(c)
        };
        vim.key(&mut host, key, opts, &mut ());
    }
    let mut shown = host.text.clone();
    shown.insert(host.cursor, '|');
    println!("{shown}");
}
