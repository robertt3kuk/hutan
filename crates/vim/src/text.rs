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
/// Columns are display columns, Vim's virtual columns: a tab reaches the next multiple of
/// `'tabstop'`, a wide character (CJK) takes two.
///
/// A text that ends in `\n` is seen without it: Vim's buffer is lines, and a file's final
/// newline ends its last line (`'eol'`) rather than starting an empty one after it. So `G`, `j`,
/// `:$`, `}` stop on the last real line, and every edit ends before that newline, which stays.
pub(crate) struct Text<'a, H: VimHost + ?Sized> {
    host: &'a H,
    tab: usize,
    /// The host's length, less the final `\n`.
    len: usize,
    /// Whether the host's text ends in a `\n` this view leaves out.
    eol: bool,
}

impl<'a, H: VimHost + ?Sized> Text<'a, H> {
    pub fn new(host: &'a H) -> Self {
        let len = host.len();
        let eol = host.char_before(len) == Some('\n');
        Self {
            host,
            tab: 8,
            len: len - usize::from(eol),
            eol,
        }
    }

    /// The same view with tabs `tab_stop` wide.
    pub fn tabs(mut self, tab_stop: usize) -> Self {
        self.tab = tab_stop.max(1);
        self
    }

    /// How many display columns `c` takes at column `col`.
    fn width_of(&self, c: char, col: usize) -> usize {
        match c {
            '\t' => self.tab - col % self.tab,
            c if c.is_control() => 2,
            c => unicode_width::UnicodeWidthChar::width(c).unwrap_or(1),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// The character starting at `i`.
    pub fn at(&self, i: usize) -> Option<char> {
        if i < self.len {
            self.host.char_at(i)
        } else {
            None
        }
    }

    /// The character ending at `i` (also past the end: a caret the host put after the final
    /// newline steps back onto the last line).
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
        self.host.line_count() - usize::from(self.eol)
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

    /// The display column where the character at `i` starts.
    pub fn column(&self, i: usize) -> usize {
        let mut col = 0;
        for c in self.host.slice(self.line_start(i)..i).chars() {
            col += self.width_of(c, col);
        }
        col
    }

    /// The character of the line starting at `ls` that covers display column `vcol`, or the
    /// line's end when the line is narrower.
    pub fn at_vcol(&self, ls: usize, vcol: usize) -> usize {
        let le = self.line_end(ls);
        let mut col = 0;
        let mut i = ls;
        while i < le {
            let Some(c) = self.at(i) else { break };
            let w = self.width_of(c, col);
            if col + w > vcol {
                return i;
            }
            col += w;
            i += c.len_utf8();
        }
        le
    }

    /// The character at display column `col` of the line starting at `ls`, or its last
    /// character when the line is narrower (its start when it is empty).
    pub fn at_column(&self, ls: usize, col: usize) -> usize {
        self.at_vcol(ls, col).min(self.last_in_line(ls))
    }

    /// The last character of the line holding `i` (its start when the line is empty).
    pub fn last_in_line(&self, i: usize) -> usize {
        let (ls, le) = (self.line_start(i), self.line_end(i));
        if le > ls { self.prev(le) } else { ls }
    }

    // ---- display rows ------------------------------------------------------------------------

    /// The display row holding `i` when the host wraps long lines (`None`: it does not).
    pub fn screen_row(&self, i: usize) -> Option<std::ops::Range<usize>> {
        self.host.display_row(i.min(self.len()))
    }

    /// The display row after (`down`) or before the one given; `None` at the end or the start
    /// of the text.
    pub fn adjacent_row(
        &self,
        row: &std::ops::Range<usize>,
        down: bool,
    ) -> Option<std::ops::Range<usize>> {
        if down {
            let line_end = self.line_end(row.start);
            if row.end < line_end {
                return self.screen_row(row.end);
            }
            if self.line_of(row.start) + 1 >= self.line_count() {
                return None;
            }
            self.screen_row(line_end + 1)
        } else {
            let line_start = self.line_start(row.start);
            if row.start > line_start {
                self.screen_row(self.prev(row.start))
            } else if line_start == 0 {
                None
            } else {
                // The line break before: the end of the line above, in its last row.
                self.screen_row(line_start - 1)
            }
        }
    }

    /// The last character of display row `row`: the line's last when it is the line's last row.
    pub fn last_in_row(&self, row: &std::ops::Range<usize>) -> usize {
        if row.end >= self.line_end(row.start) {
            self.last_in_line(row.start)
        } else {
            self.prev(row.end)
        }
        .max(row.start)
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

    // ---- lines as a whole ----------------------------------------------------------------------

    /// The text of `line` without its newline.
    pub fn line_text(&self, line: usize) -> std::borrow::Cow<'a, str> {
        let ls = self.line_start_of(line);
        self.host.slice(ls..self.line_end(ls))
    }

    pub fn is_empty_line(&self, line: usize) -> bool {
        let ls = self.line_start_of(line);
        ls == self.line_end(ls)
    }

    /// Whether `line` holds nothing but spaces and tabs (Vim's "white" lines of `ip`).
    pub fn is_white_line(&self, line: usize) -> bool {
        let ls = self.line_start_of(line);
        let le = self.line_end(ls);
        let mut i = ls;
        while i < le {
            match self.at(i) {
                Some(' ' | '\t') => i += 1,
                _ => return false,
            }
        }
        true
    }

    /// `g_`: the last character of the line starting at `ls` that is not blank (its start when
    /// it has none).
    pub fn last_nonblank(&self, ls: usize) -> usize {
        let mut i = self.line_end(ls);
        while i > ls {
            let p = self.prev(i);
            if !matches!(self.at(p), Some(' ' | '\t')) {
                return p;
            }
            i = p;
        }
        ls
    }

    /// How many display columns `s` takes from column 0.
    pub fn width_of_str(&self, s: &str) -> usize {
        let mut col = 0;
        for c in s.chars() {
            col += self.width_of(c, col);
        }
        col
    }

    /// How many display columns `line` takes.
    pub fn line_width(&self, line: usize) -> usize {
        let ls = self.line_start_of(line);
        self.column(self.line_end(ls))
    }

    /// One character back the way Normal mode's caret moves: from a line's start onto the last
    /// character of the line before (or that line's start when it is empty). `None` at the
    /// start of the text.
    pub fn dec(&self, i: usize) -> Option<usize> {
        if i == 0 {
            return None;
        }
        let p = self.prev(i);
        if self.at(p) != Some('\n') {
            return Some(p);
        }
        let ls = self.line_start(p);
        Some(if ls == p { p } else { self.prev(p) })
    }

    /// `ge` / `gE`: the end of the word before `from`; `None` at the start of the text. A line
    /// break counts as a blank, an empty line as a word.
    pub fn word_end_back(&self, from: usize, big: bool) -> Option<usize> {
        let cls = |i: usize| {
            self.at(i)
                .filter(|c| *c != '\n')
                .map_or(0, |c| class(c, big))
        };
        let back = |i: usize| (i > 0).then(|| self.prev(i));
        let start_class = cls(from);
        let mut i = back(from)?;
        if start_class != 0 {
            while cls(i) == start_class {
                match back(i) {
                    Some(p) => i = p,
                    None => return Some(i),
                }
            }
        }
        while cls(i) == 0 {
            if self.line_start(i) == i && self.is_empty_line(self.line_of(i)) {
                break;
            }
            match back(i) {
                Some(p) => i = p,
                None => return Some(i),
            }
        }
        Some(i)
    }

    // ---- paragraphs, sections, sentences --------------------------------------------------------

    /// Whether `line` starts a paragraph (`what` none: an empty line) or a section (`what` `{`
    /// or `}` in its first column; `both` takes `}` too); a form feed starts both.
    fn starts_par(&self, line: usize, what: Option<char>, both: bool) -> bool {
        let first = self.at(self.line_start_of(line));
        let first = first.filter(|c| *c != '\n');
        match first {
            None => what.is_none(),
            Some('\x0c') => true,
            Some(c) => Some(c) == what || (both && c == '}'),
        }
    }

    /// `{` `}` (`what` none) and `[[` `]]` `[]` `][` (`what` `{` or `}`): the `count`-th
    /// boundary line from the line of `from`. On the last line (moving forward, not `][`) the
    /// target is the last character, inclusive. `None` when the text ends with count left.
    pub fn paragraph(
        &self,
        from: usize,
        forward: bool,
        count: usize,
        what: Option<char>,
        both: bool,
    ) -> Option<Target> {
        let last = self.line_count() - 1;
        let mut curr = self.line_of(from);
        let mut left = count.max(1);
        while left > 0 {
            left -= 1;
            let mut did_skip = false;
            let mut first = true;
            loop {
                if !self.is_empty_line(curr) {
                    did_skip = true;
                }
                if !first && did_skip && self.starts_par(curr, what, both) {
                    break;
                }
                let next = if forward {
                    (curr < last).then(|| curr + 1)
                } else {
                    curr.checked_sub(1)
                };
                match next {
                    Some(n) => curr = n,
                    None if left > 0 => return None,
                    None => break,
                }
                first = false;
            }
        }
        if both && self.at(self.line_start_of(curr)) == Some('}') && curr < last {
            curr += 1;
        }
        let ls = self.line_start_of(curr);
        if curr == last && what.is_none() && forward {
            let le = self.line_end(ls);
            if le > ls {
                return Some(Target {
                    to: self.prev(le),
                    kind: Kind::Inclusive,
                });
            }
        }
        Some(Target {
            to: ls,
            kind: Kind::Exclusive,
        })
    }

    /// Whether a sentence ends just before `i` (`.` `!` `?`, then any of `)` `]` `"` `'`), with
    /// a blank or the line's end at `i`.
    fn sentence_end_before(&self, i: usize) -> bool {
        if !matches!(self.at(i), None | Some(' ' | '\t' | '\n')) {
            return false;
        }
        let mut j = i;
        while j > 0 {
            match self.before(j) {
                Some(')' | ']' | '"' | '\'') => j = self.prev(j),
                Some('.' | '!' | '?') => return true,
                _ => return false,
            }
        }
        false
    }

    /// The starts of the sentences, in text order, from the paragraph before `around` to the
    /// one after it: the first non-blank after a sentence end, the first non-blank of a
    /// paragraph, and every empty line.
    fn sentence_starts(&self, around: usize) -> Vec<usize> {
        let len = self.len();
        // From the empty line (or the start) before the paragraph of `around`.
        let mut line = self.line_of(around);
        while line > 0 && !self.is_empty_line(line) {
            line -= 1;
        }
        if line > 0 {
            line -= 1;
            while line > 0 && !self.is_empty_line(line) {
                line -= 1;
            }
        }
        let mut i = self.line_start_of(line);
        let mut out = Vec::new();
        // Whether the next non-blank starts a sentence.
        let mut starts = true;
        let mut passed_paragraph_after = 0;
        while i < len {
            let c = self.at(i).unwrap_or('\n');
            if c == '\n' {
                let ls = self.line_start(i);
                if ls == i {
                    // An empty line.
                    out.push(i);
                    starts = true;
                    if i > around {
                        passed_paragraph_after += 1;
                        if passed_paragraph_after > 1 {
                            break;
                        }
                    }
                } else if self.sentence_end_before(i) {
                    starts = true;
                }
                i += 1;
                continue;
            }
            if c == ' ' || c == '\t' {
                if self.sentence_end_before(i) {
                    starts = true;
                }
                i += 1;
                continue;
            }
            if starts {
                out.push(i);
                starts = false;
            }
            i = self.next(i);
        }
        out
    }

    /// `)` (`forward`) and `(`: the start of the `count`-th next (previous) sentence.
    pub fn sentence(&self, from: usize, forward: bool, count: usize) -> usize {
        let mut at = from;
        for _ in 0..count.max(1) {
            let starts = self.sentence_starts(at);
            at = if forward {
                match starts.iter().find(|&&s| s > at) {
                    Some(&s) => s,
                    None => {
                        let last = self.line_start_of(self.line_count() - 1);
                        let le = self.line_end(last);
                        if le > last { self.prev(le) } else { last }
                    }
                }
            } else {
                starts.iter().rev().find(|&&s| s < at).copied().unwrap_or(0)
            };
        }
        at
    }

    /// `is` / `as`: the sentence at `from` (with the blanks after it, or before it when none
    /// follow, for `as`), `count` sentences. End exclusive.
    pub fn sentence_object(
        &self,
        from: usize,
        around: bool,
        count: usize,
    ) -> Option<(usize, usize)> {
        if self.at(from).is_none_or(|c| c == '\n') && self.is_empty_line(self.line_of(from)) {
            return None;
        }
        let starts = self.sentence_starts(from);
        let start = starts
            .iter()
            .rev()
            .find(|&&s| s <= from)
            .copied()
            .unwrap_or(0);
        let in_blank = matches!(self.at(from), Some(' ' | '\t'))
            && starts.iter().all(|&s| {
                s > from || s < self.line_start(from) || {
                    // Blanks after the end of the sentence that started at `s`.
                    let mut j = from;
                    while matches!(self.at(j), Some(' ' | '\t')) && j > s {
                        j = self.prev(j);
                    }
                    self.sentence_end_before(self.next(j))
                }
            });
        let mut end = from;
        let mut start = start;
        for n in 0..count.max(1) {
            // The end of the sentence (after its punctuation and closers).
            let next = starts.iter().find(|&&s| s > end).copied();
            let stop = next.unwrap_or(self.len());
            let mut e = stop;
            while e > end && matches!(self.before(e), Some(' ' | '\t' | '\n')) {
                e = self.prev(e);
            }
            if n == 0 && in_blank {
                // On the blanks between sentences: they are the "sentence".
                start = e;
                e = stop;
                while e > start && self.before(e) == Some('\n') {
                    e = self.prev(e);
                }
            }
            end = if around && !(n == 0 && in_blank) {
                let mut t = e;
                while matches!(self.at(t), Some(' ' | '\t')) {
                    t = self.next(t);
                }
                t
            } else if around && n == 0 && in_blank {
                // The blanks and the sentence after them.
                let after = starts.iter().find(|&&s| s >= e).copied().unwrap_or(e);
                let following = starts
                    .iter()
                    .find(|&&s| s > after)
                    .copied()
                    .unwrap_or(self.len());
                let mut t = following;
                while t > after && matches!(self.before(t), Some(' ' | '\t' | '\n')) {
                    t = self.prev(t);
                }
                t
            } else {
                e
            };
        }
        if around && !in_blank && !matches!(self.before(end), Some(' ' | '\t')) {
            // No blanks after it: take the ones before it.
            while start > self.line_start(start) && matches!(self.before(start), Some(' ' | '\t')) {
                start = self.prev(start);
            }
        }
        (end > start).then_some((start, end))
    }

    /// `ip` / `ap`: the lines (first, last) of the paragraph at `line`, `count` of them.
    pub fn paragraph_object(
        &self,
        line: usize,
        around: bool,
        count: usize,
    ) -> Option<(usize, usize)> {
        let last = self.line_count() - 1;
        let white = self.is_white_line(line);
        let mut start = line;
        while start > 0 {
            if white {
                if !self.is_white_line(start - 1) {
                    break;
                }
            } else if self.is_white_line(start - 1) {
                break;
            }
            start -= 1;
        }
        // Past the white lines at the start.
        let mut end = start;
        while end <= last && self.is_white_line(end) {
            end += 1;
        }
        // `end` is one past; work with "the last line taken" as `end - 1` (may be before start).
        let mut taken: isize = end as isize - 1;
        let mut left = count.max(1);
        if !around && white {
            left -= 1;
        }
        let mut do_white = false;
        while left > 0 {
            left -= 1;
            if taken == last as isize {
                return None;
            }
            if !around {
                do_white = self.is_white_line((taken + 1) as usize);
            }
            if around || !do_white {
                taken += 1;
                while (taken as usize) < last && !self.is_white_line(taken as usize + 1) {
                    taken += 1;
                }
            }
            if left == 0 && white && around {
                break;
            }
            if around || do_white {
                while (taken as usize) < last && self.is_white_line(taken as usize + 1) {
                    taken += 1;
                }
            }
        }
        if !white && !do_white && around {
            // No white lines after it: take the ones before it.
            let had_after = taken as usize > line && self.is_white_line(taken as usize);
            if !had_after {
                while start > 0 && self.is_white_line(start - 1) {
                    start -= 1;
                }
            }
        }
        Some((start, taken.max(start as isize) as usize))
    }

    // ---- brackets and quotes -------------------------------------------------------------------

    /// `%`: the bracket that matches the first of `( ) [ ] { }` at or after `from` on its line.
    pub fn percent(&self, from: usize) -> Option<usize> {
        let le = self.line_end(from);
        let mut i = from;
        while i < le {
            if let Some(c) = self.at(i)
                && let Some((open, close, forward)) = bracket_pair(c)
            {
                return self.match_bracket(i, open, close, forward);
            }
            i = self.next(i);
        }
        None
    }

    /// The bracket that matches the one at `at`, counting nested pairs.
    pub fn match_bracket(
        &self,
        at: usize,
        open: char,
        close: char,
        forward: bool,
    ) -> Option<usize> {
        let mut depth = 0usize;
        let mut i = at;
        loop {
            let c = self.at(i)?;
            if c == open || c == close {
                let opening = (c == open) == forward;
                if opening {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
            }
            if forward {
                i = self.next(i);
                if i >= self.len() {
                    return None;
                }
            } else {
                if i == 0 {
                    return None;
                }
                i = self.prev(i);
            }
        }
    }

    /// The `count`-th pair of `open`/`close` around `from` (the caret on either bracket counts as
    /// inside its pair): the offsets of both brackets.
    pub fn enclosing(
        &self,
        from: usize,
        open: char,
        close: char,
        count: usize,
    ) -> Option<(usize, usize)> {
        let mut at = from;
        let mut pair = None;
        for n in 0..count.max(1) {
            let start = match self.at(at) {
                Some(c) if c == open && n == 0 => at,
                Some(c) if c == close && n == 0 => self.match_bracket(at, open, close, false)?,
                _ => {
                    // The unmatched `open` before `at`.
                    let mut depth = 0usize;
                    let mut i = at;
                    loop {
                        if i == 0 {
                            return None;
                        }
                        i = self.prev(i);
                        let c = self.at(i)?;
                        if c == close {
                            depth += 1;
                        } else if c == open {
                            if depth == 0 {
                                break i;
                            }
                            depth -= 1;
                        }
                    }
                }
            };
            let end = self.match_bracket(start, open, close, true)?;
            pair = Some((start, end));
            at = start;
        }
        pair
    }

    /// `i(` and friends: the inside of a pair as an operator takes it, `(start, end)` end
    /// exclusive. When the opening bracket ends its line, the inside starts on the next line;
    /// when only blanks precede the closing one on its line, it ends at the start of that line
    /// (an exclusive end in the first column: the operator then takes whole lines).
    pub fn inner_block(&self, open_at: usize, close_at: usize) -> (usize, usize) {
        let mut start = self.next(open_at);
        if self.at(start) == Some('\n') && start < close_at {
            start += 1;
        }
        let ls = self.line_start(close_at);
        let mut end = close_at;
        let mut only_blanks = true;
        let mut i = ls;
        while i < close_at {
            if !matches!(self.at(i), Some(' ' | '\t')) {
                only_blanks = false;
                break;
            }
            i += 1;
        }
        if only_blanks && ls > start {
            end = ls;
        }
        (start.min(end), end)
    }

    /// `i"` `a"` (and `'`, `` ` ``) on the line of `from`: `(start, end)`, end exclusive.
    pub fn quote_object(
        &self,
        from: usize,
        quote: char,
        around: bool,
        count: usize,
    ) -> Option<(usize, usize)> {
        let ls = self.line_start(from);
        let line = self.line_text(self.line_of(from));
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let col = chars.iter().position(|(o, _)| ls + o == from)?;
        let next_quote = |from: usize, escapes: bool| -> Option<usize> {
            let mut i = from;
            while i < chars.len() {
                let c = chars[i].1;
                if escapes && c == '\\' {
                    i += 2;
                    continue;
                }
                if c == quote {
                    return Some(i);
                }
                i += 1;
            }
            None
        };
        let (start, end) = if chars[col].1 == quote {
            let mut start = 0;
            loop {
                let s = next_quote(start, false)?;
                if s > col {
                    return None;
                }
                let e = next_quote(s + 1, true)?;
                if s <= col && col <= e {
                    break (s, e);
                }
                start = e + 1;
            }
        } else {
            // The quote before the caret (not an escaped one), else the first one after it.
            let mut s = col;
            let mut found = None;
            while s > 0 {
                s -= 1;
                let mut n = 0;
                while s > n && chars[s - n - 1].1 == '\\' {
                    n += 1;
                }
                if n % 2 == 1 {
                    s -= n;
                } else if chars[s].1 == quote {
                    found = Some(s);
                    break;
                }
            }
            let s = match found {
                Some(s) => s,
                None => next_quote(col, false)?,
            };
            (s, next_quote(s + 1, true)?)
        };
        let mut start = start;
        let mut end = end;
        let blank = |i: usize| chars.get(i).is_some_and(|(_, c)| *c == ' ' || *c == '\t');
        if around {
            if blank(end + 1) {
                while blank(end + 1) {
                    end += 1;
                }
            } else {
                while start > 0 && blank(start - 1) {
                    start -= 1;
                }
            }
        }
        let offset = |i: usize| chars.get(i).map_or(ls + line.len(), |(o, _)| ls + o);
        if !around && count < 2 {
            Some((offset(start + 1), offset(end)))
        } else {
            Some((offset(start), offset(end + 1)))
        }
    }

    /// The keyword under or after `from` on its line (`*`, `#`), else the non-blank run there:
    /// `(start, end, keyword)`.
    pub fn word_for_star(&self, from: usize) -> Option<(usize, usize, bool)> {
        let le = self.line_end(from);
        let keyword = |c: char| class(c, false) == 2;
        let mut i = from;
        while i < le && !self.at(i).is_some_and(keyword) {
            i = self.next(i);
        }
        let (pick, is_kw): (&dyn Fn(char) -> bool, bool) = if i < le {
            (&keyword, true)
        } else {
            i = from;
            while i < le && self.at(i).is_some_and(|c| c == ' ' || c == '\t') {
                i = self.next(i);
            }
            if i >= le {
                return None;
            }
            (&|c: char| c != ' ' && c != '\t' && c != '\n', false)
        };
        let ls = self.line_start(from);
        let mut start = i;
        while start > ls && self.before(start).is_some_and(pick) {
            start = self.prev(start);
        }
        let mut end = i;
        while end < le && self.at(end).is_some_and(pick) {
            end = self.next(end);
        }
        Some((start, end, is_kw))
    }
}

/// `(open, close, forward)` of a bracket character.
pub(crate) fn bracket_pair(c: char) -> Option<(char, char, bool)> {
    Some(match c {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        _ => return None,
    })
}
