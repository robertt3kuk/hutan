//! Motions and text objects over a [`VimHost`], in byte offsets.
//!
//! [`Text`] is a view of the host's current text: it owns nothing, so it is built for every key
//! (free) and never outlives an edit.

use crate::host::VimHost;

/// `f`, `t`, `F`, `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Find {
    pub forward: bool,
    pub till: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Target {
    pub to: usize,
    pub kind: Kind,
}

/// The text an operator acts on: byte offsets, `end` exclusive.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub linewise: bool,
}

/// 0 blank, 1 punctuation, 2 word (everything non-blank with `big`).
pub(crate) fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        2
    } else {
        1
    }
}

/// The host's text, for motions in byte offsets that always sit on character boundaries.
pub(crate) struct Text<'a, H: VimHost + ?Sized> {
    host: &'a H,
}

impl<'a, H: VimHost + ?Sized> Text<'a, H> {
    pub fn new(host: &'a H) -> Self {
        Self { host }
    }

    pub fn len(&self) -> usize {
        self.host.len()
    }

    /// The character starting at `i`.
    pub fn at(&self, i: usize) -> Option<char> {
        self.host.char_at(i)
    }

    /// The character ending at `i`.
    pub fn before(&self, i: usize) -> Option<char> {
        self.host.char_before(i)
    }

    /// The offset after the character at `i` (`i` itself at the end).
    pub fn next(&self, i: usize) -> usize {
        self.at(i).map_or(i, |c| i + c.len_utf8())
    }

    /// The offset of the character before `i` (`i` itself at the start).
    pub fn prev(&self, i: usize) -> usize {
        self.before(i).map_or(i, |c| i - c.len_utf8())
    }

    /// `i`, or the next character boundary after it when it is inside a character.
    pub fn boundary_up(&self, i: usize) -> usize {
        let mut i = i.min(self.len());
        while i < self.len() && self.at(i).is_none() {
            i += 1;
        }
        i
    }

    /// Steps `n` characters on from `from`, stopping at `limit`.
    pub fn advance(&self, from: usize, n: usize, limit: usize) -> usize {
        let mut to = from;
        for _ in 0..n {
            if to >= limit {
                break;
            }
            to = self.next(to);
        }
        to
    }

    /// Steps `n` characters back from `from`, stopping at `limit`.
    pub fn retreat(&self, from: usize, n: usize, limit: usize) -> usize {
        let mut to = from;
        for _ in 0..n {
            if to <= limit {
                break;
            }
            to = self.prev(to);
        }
        to
    }

    // ---- lines -------------------------------------------------------------------------------

    pub fn line_of(&self, i: usize) -> usize {
        self.host.line_of(i.min(self.len()))
    }

    pub fn line_count(&self) -> usize {
        self.host.line_count()
    }

    /// The start of line `line`, or of the last line when there are fewer.
    pub fn line_start_of(&self, line: usize) -> usize {
        self.host.line_start(line.min(self.line_count() - 1))
    }

    pub fn line_start(&self, i: usize) -> usize {
        self.host.line_start_at(i.min(self.len()))
    }

    /// The offset of the `\n` ending the line (or the text's end).
    pub fn line_end(&self, i: usize) -> usize {
        self.host.line_end_at(i.min(self.len()))
    }

    /// How many characters lie between the start of `i`'s line and `i`.
    pub fn column(&self, i: usize) -> usize {
        self.host.slice(self.line_start(i)..i).chars().count()
    }

    /// The character at column `col` of the line starting at `ls`, or its last character when
    /// the line is shorter (its start when it is empty).
    pub fn at_column(&self, ls: usize, col: usize) -> usize {
        self.advance(ls, col, self.last_in_line(ls))
    }

    /// The last character of the line holding `i` (its start when the line is empty).
    pub fn last_in_line(&self, i: usize) -> usize {
        let (ls, le) = (self.line_start(i), self.line_end(i));
        if le > ls { self.prev(le) } else { ls }
    }

    /// The first character of the line starting at `ls` that is not a space or tab, or its last
    /// character when it is all blank.
    pub fn first_nonblank(&self, ls: usize) -> usize {
        let le = self.line_end(ls);
        let mut i = ls;
        while i < le && matches!(self.at(i), Some(' ' | '\t')) {
            i += 1;
        }
        if i == le && le > ls { self.prev(le) } else { i }
    }

    /// Normal mode's caret cannot rest on the line's end, except on an empty line.
    pub fn normal_clamp(&self, i: usize) -> usize {
        let i = i.min(self.len());
        let (ls, le) = (self.line_start(i), self.line_end(i));
        if i >= le && le > ls { self.prev(le) } else { i }
    }

    // ---- words -------------------------------------------------------------------------------

    fn class_at(&self, i: usize, big: bool) -> Option<u8> {
        self.at(i).map(|c| class(c, big))
    }

    /// `w`: the start of the next word.
    pub fn word_forward(&self, from: usize, big: bool) -> usize {
        let len = self.len();
        let mut i = from;
        if let Some(cls) = self.class_at(i, big)
            && cls != 0
        {
            while self.class_at(i, big) == Some(cls) {
                i = self.next(i);
            }
        }
        while i < len {
            let Some(c) = self.at(i) else { break };
            if c == '\n' {
                // An empty line is a word of its own.
                if i > from && self.before(i) == Some('\n') {
                    break;
                }
                i += 1;
            } else if class(c, big) == 0 {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        i
    }

    /// `b`: the start of the word before `from`.
    pub fn word_back(&self, from: usize, big: bool) -> usize {
        if from == 0 {
            return 0;
        }
        let mut i = self.prev(from);
        while i > 0 && self.class_at(i, big) == Some(0) {
            if self.at(i) == Some('\n') && self.before(i) == Some('\n') {
                return i;
            }
            i = self.prev(i);
        }
        let cls = self.class_at(i, big).unwrap_or(0);
        if cls == 0 {
            return i;
        }
        while i > 0 && self.before(i).map(|c| class(c, big)) == Some(cls) {
            i = self.prev(i);
        }
        i
    }

    /// `e`: the end of the word after `from`.
    pub fn word_end(&self, from: usize, big: bool) -> usize {
        let len = self.len();
        if len == 0 {
            return 0;
        }
        let mut i = self.next(from);
        while self.class_at(i, big) == Some(0) {
            i = self.next(i);
        }
        if i >= len {
            return self.prev(len);
        }
        self.run_end(i, big)
    }

    /// The last character of the run (of the same class) that holds `from`.
    pub fn run_end(&self, from: usize, big: bool) -> usize {
        let Some(c) = self.at(from) else {
            return from;
        };
        let cls = class(c, big);
        let mut i = from;
        loop {
            let next = self.next(i);
            match self.at(next) {
                Some(c) if class(c, big) == cls && c != '\n' => i = next,
                _ => return i,
            }
        }
    }

    /// `iw` / `aw` around `from`: `(start, end)`, end exclusive.
    pub fn word_object(&self, from: usize, around: bool, big: bool) -> Option<(usize, usize)> {
        let c = self.at(from)?;
        if c == '\n' {
            return None;
        }
        let cls = class(c, big);
        let same = |c: Option<char>| c.is_some_and(|c| c != '\n' && class(c, big) == cls);
        let mut start = from;
        while same(self.before(start)) {
            start = self.prev(start);
        }
        let mut end = self.next(from);
        while same(self.at(end)) {
            end = self.next(end);
        }
        if !around {
            return Some((start, end));
        }
        let blank = |c: Option<char>| c.is_some_and(|c| c != '\n' && class(c, big) == 0);
        if cls == 0 {
            // Blanks, and the word after them.
            if let Some(next) = self.at(end).filter(|c| *c != '\n') {
                let next_cls = class(next, big);
                while self
                    .at(end)
                    .is_some_and(|c| c != '\n' && class(c, big) == next_cls)
                {
                    end = self.next(end);
                }
            }
        } else if blank(self.at(end)) {
            while blank(self.at(end)) {
                end = self.next(end);
            }
        } else {
            while blank(self.before(start)) {
                start = self.prev(start);
            }
        }
        Some((start, end))
    }

    /// `f`, `t`, `F`, `T`: the `n`-th `ch` after (before) `from` on its line.
    pub fn find(&self, from: usize, find: Find, ch: char, n: usize) -> Option<usize> {
        let (ls, le) = (self.line_start(from), self.line_end(from));
        let mut left = n;
        if find.forward {
            let mut i = self.next(from);
            while i < le {
                if self.at(i) == Some(ch) {
                    left -= 1;
                    if left == 0 {
                        return Some(if find.till { self.prev(i) } else { i });
                    }
                }
                i = self.next(i);
            }
        } else {
            let mut i = from;
            while i > ls {
                i = self.prev(i);
                if self.at(i) == Some(ch) {
                    left -= 1;
                    if left == 0 {
                        return Some(if find.till { self.next(i) } else { i });
                    }
                }
            }
        }
        None
    }

    /// The text an operator takes for a motion from `from` to `target`.
    pub fn span(&self, from: usize, target: Target, single_line: bool) -> Span {
        let (lo, hi) = (from.min(target.to), from.max(target.to));
        match target.kind {
            Kind::Exclusive => Span {
                start: lo,
                end: hi,
                linewise: false,
            },
            Kind::Inclusive => Span {
                start: lo,
                end: if hi < self.len() && self.at(hi) != Some('\n') {
                    self.next(hi)
                } else {
                    hi
                },
                linewise: false,
            },
            Kind::Linewise if single_line => Span {
                start: 0,
                end: self.len(),
                linewise: false,
            },
            Kind::Linewise => Span {
                start: self.line_start(lo),
                end: self.line_end(hi),
                linewise: true,
            },
        }
    }
}
