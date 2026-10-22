//! Normal and Visual mode commands: motions (with search), the commands that are not
//! operators, and the Visual selection.

use std::ops::Range;

use crate::command::{InsertAt, Motion, Object, Simple};
use crate::host::{Clipboard, Transaction, VimHost};
use crate::key::{Mode, Options};
use crate::marks::Pos;
use crate::pattern::Pattern;
use crate::text::{Find, Kind, Target, Text};
use crate::vim::{BlockInsert, Core, LastSearch, Repeat, VisualMemory};

/// A Visual block: lines and display columns, both ends included (`right` is `usize::MAX`
/// after `$`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Block {
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

impl Block {
    /// The bytes of `line` inside the block; `None` when the line ends before it.
    pub fn row<H: VimHost + ?Sized>(&self, buf: &Text<'_, H>, line: usize) -> Option<Range<usize>> {
        let ls = buf.line_start_of(line);
        let le = buf.line_end(ls);
        if self.left >= buf.line_width(line) {
            return None;
        }
        let start = buf.at_vcol(ls, self.left);
        let end = if self.right == usize::MAX {
            le
        } else {
            buf.at_vcol(ls, self.right + 1)
        };
        Some(start..end.max(start))
    }

    /// How wide the block is, in display columns.
    pub fn width<H: VimHost + ?Sized>(&self, buf: &Text<'_, H>) -> usize {
        if self.right == usize::MAX {
            (self.top..=self.bottom)
                .map(|l| buf.line_width(l).saturating_sub(self.left))
                .max()
                .unwrap_or(0)
        } else {
            self.right + 1 - self.left
        }
    }
}

impl Core {
    // ---- motions -------------------------------------------------------------------------------

    pub fn do_move<H: VimHost>(
        &mut self,
        host: &mut H,
        motion: &Motion,
        count: Option<usize>,
        opts: Options,
    ) {
        let from = host.cursor();
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.want_col = None;
        }
        let Some(target) = self.target(host, motion, count, false, opts) else {
            self.fail();
            return;
        };
        if motion.is_jump() {
            self.marks.jump_from(Self::pos(host, from));
        }
        if *motion == Motion::LineEnd {
            self.want_col = Some(usize::MAX);
        }
        let buf = Text::new(&*host);
        let to = buf.normal_clamp(target.to);
        host.set_cursor(to);
    }

    /// Where `motion` leads from the caret; `None` when it cannot move (the command fails).
    /// `op` is whether an operator waits for it (a few motions differ then).
    pub fn target<H: VimHost>(
        &mut self,
        host: &H,
        motion: &Motion,
        count: Option<usize>,
        op: bool,
        opts: Options,
    ) -> Option<Target> {
        let buf = self.text(host);
        let from = host.cursor();
        let n = count.unwrap_or(1).max(1);
        let exclusive = |to| {
            Some(Target {
                to,
                kind: Kind::Exclusive,
            })
        };
        let inclusive = |to| {
            Some(Target {
                to,
                kind: Kind::Inclusive,
            })
        };
        let linewise = |to| {
            Some(Target {
                to,
                kind: Kind::Linewise,
            })
        };
        let last_line = buf.line_count() - 1;
        let current = buf.line_of(from);
        match motion {
            Motion::Left => {
                let to = buf.retreat(from, n, buf.line_start(from));
                if to == from && !op {
                    return None;
                }
                exclusive(to)
            }
            Motion::Right => {
                let limit = if op {
                    buf.line_end(from)
                } else {
                    buf.last_in_line(from).max(from)
                };
                let to = buf.advance(from, n, limit);
                if to == from && !op {
                    return None;
                }
                exclusive(to)
            }
            Motion::Up | Motion::Down => {
                if opts.single_line {
                    return None;
                }
                let line = if *motion == Motion::Up {
                    if current == 0 {
                        return None;
                    }
                    current.saturating_sub(n)
                } else {
                    if current >= last_line {
                        return None;
                    }
                    (current + n).min(last_line)
                };
                let col = *self.want_col.get_or_insert_with(|| buf.column(from));
                linewise(buf.at_column(buf.line_start_of(line), col))
            }
            Motion::LineDown | Motion::LineUp | Motion::LineHere => {
                let line = match motion {
                    Motion::LineUp => {
                        if current == 0 {
                            return None;
                        }
                        current.saturating_sub(n)
                    }
                    Motion::LineDown => {
                        if current >= last_line {
                            return None;
                        }
                        (current + n).min(last_line)
                    }
                    _ => {
                        if n > 1 && current >= last_line {
                            return None;
                        }
                        (current + n - 1).min(last_line)
                    }
                };
                linewise(buf.first_nonblank(buf.line_start_of(line)))
            }
            Motion::Word { big } => {
                let mut to = from;
                let mut last_start = from;
                for _ in 0..n {
                    last_start = to;
                    to = buf.word_forward(to, *big);
                }
                if op && to > from && buf.line_of(to) > buf.line_of(last_start) {
                    // The last word of a line: the operator stops at the line's end.
                    to = buf.line_end(last_start);
                } else if !op && to >= buf.len() {
                    // Vim moves onto the last character and beeps.
                    self.fail();
                }
                exclusive(to)
            }
            Motion::Back { big } => {
                let mut to = from;
                for _ in 0..n {
                    if to == 0 {
                        if to == from {
                            return None;
                        }
                        self.fail();
                        break;
                    }
                    to = buf.word_back(to, *big);
                }
                exclusive(to)
            }
            Motion::End { big } => {
                let mut to = from;
                for _ in 0..n {
                    let next = buf.word_end(to, *big);
                    if next <= to {
                        if to == from {
                            return None;
                        }
                        self.fail();
                        break;
                    }
                    to = next;
                }
                inclusive(to)
            }
            Motion::BackEnd { big } => {
                let mut to = from;
                for _ in 0..n {
                    match buf.word_end_back(to, *big) {
                        Some(t) => to = t,
                        None if to == from => return None,
                        None => break,
                    }
                }
                inclusive(to)
            }
            Motion::LineStart => exclusive(buf.line_start(from)),
            Motion::FirstNonBlank => exclusive(buf.first_nonblank(buf.line_start(from))),
            Motion::LineEnd => {
                if n > 1 && current >= last_line {
                    return None;
                }
                let line = (current + n - 1).min(last_line);
                inclusive(buf.last_in_line(buf.line_start_of(line)))
            }
            Motion::LastNonBlank => {
                if n > 1 && current >= last_line {
                    return None;
                }
                let line = (current + n - 1).min(last_line);
                inclusive(buf.last_nonblank(buf.line_start_of(line)))
            }
            Motion::Column => exclusive(buf.at_column(buf.line_start(from), n - 1)),
            Motion::FileStart | Motion::FileEnd => {
                let line = match (count, motion) {
                    (Some(n), _) => (n.max(1) - 1).min(last_line),
                    (None, Motion::FileStart) => 0,
                    (None, _) => last_line,
                };
                linewise(buf.first_nonblank(buf.line_start_of(line)))
            }
            Motion::Find(find, ch) => {
                self.last_find = Some((*find, *ch));
                buf.find(from, *find, *ch, n).map(|to| Target {
                    to,
                    kind: if find.forward {
                        Kind::Inclusive
                    } else {
                        Kind::Exclusive
                    },
                })
            }
            Motion::RepeatFind { reverse } => {
                let (find, ch) = self.last_find?;
                let find = Find {
                    forward: find.forward != *reverse,
                    till: find.till,
                };
                // A repeated `t` would stay in place before the same character: step over it.
                let start = match (find.till, find.forward) {
                    (true, true) if buf.at(buf.next(from)) == Some(ch) => buf.next(from),
                    (true, false) if from > 0 && buf.before(from) == Some(ch) => buf.prev(from),
                    _ => from,
                };
                buf.find(start, find, ch, n).map(|to| Target {
                    to,
                    kind: if find.forward {
                        Kind::Inclusive
                    } else {
                        Kind::Exclusive
                    },
                })
            }
            Motion::Percent => match count {
                Some(percent) => {
                    if percent > 100 {
                        return None;
                    }
                    let lines = buf.line_count();
                    let line = (percent * lines).div_ceil(100).max(1) - 1;
                    linewise(buf.first_nonblank(buf.line_start_of(line)))
                }
                None => buf.percent(from).map(|to| Target {
                    to,
                    kind: Kind::Inclusive,
                }),
            },
            Motion::Paragraph { forward } => buf.paragraph(from, *forward, n, None, false),
            Motion::Section { forward, close } => buf.paragraph(
                from,
                *forward,
                n,
                Some(if *close { '}' } else { '{' }),
                op && *forward && !*close,
            ),
            Motion::Sentence { forward } => exclusive(buf.sentence(from, *forward, n)),
            Motion::Mark { name, exact } => {
                let Some(pos) = self.marks.get(*name) else {
                    self.error("E20: Mark not set");
                    return None;
                };
                if pos.line > last_line {
                    self.error("E19: Mark has invalid line number");
                    return None;
                }
                let at = Self::offset(host, pos);
                if *exact {
                    exclusive(at)
                } else {
                    linewise(buf.first_nonblank(buf.line_start(at)))
                }
            }
            Motion::Search { forward, text } => {
                let (pattern, offset) = split_offset(text, if *forward { '/' } else { '?' });
                let pattern = if pattern.is_empty() {
                    match &self.last_search {
                        Some(s) => s.pattern.clone(),
                        None => {
                            self.error("E35: No previous regular expression");
                            return None;
                        }
                    }
                } else {
                    pattern
                };
                self.last_search = Some(LastSearch {
                    pattern: pattern.clone(),
                    forward: *forward,
                    offset: offset.clone(),
                });
                self.regs.last_search = Some(pattern.clone());
                self.search_target(host, &pattern, *forward, &offset, n, None)
            }
            Motion::SearchNext { reverse } => {
                let Some(last) = self.last_search.clone() else {
                    self.error("E35: No previous regular expression");
                    return None;
                };
                self.search_target(
                    host,
                    &last.pattern,
                    last.forward != *reverse,
                    &last.offset,
                    n,
                    None,
                )
            }
            Motion::Star { forward, whole } => {
                let Some((start, end, keyword)) = buf.word_for_star(from) else {
                    self.error("E348: No string under cursor");
                    return None;
                };
                let word = host.slice(start..end).into_owned();
                let mut pattern = String::new();
                for c in word.chars() {
                    if matches!(c, '\\' | '/' | '.' | '*' | '$' | '^' | '~' | '[' | ']') {
                        pattern.push('\\');
                    }
                    pattern.push(c);
                }
                if keyword && *whole {
                    pattern = format!("\\<{pattern}\\>");
                }
                self.last_search = Some(LastSearch {
                    pattern: pattern.clone(),
                    forward: *forward,
                    offset: String::new(),
                });
                self.regs.last_search = Some(pattern.clone());
                // From the start of the word, so that `*` finds the next one.
                let ignore = Some(self.settings.ignore_case);
                self.search_from(host, &pattern, *forward, start, n, ignore)
                    .map(|to| Target {
                        to,
                        kind: Kind::Exclusive,
                    })
            }
        }
    }

    fn compile(&mut self, pattern: &str, ignore_case: Option<bool>) -> Option<Pattern> {
        match Pattern::new(
            pattern,
            ignore_case,
            self.settings.case(),
            self.last_replacement.as_deref(),
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                self.error(e.0);
                None
            }
        }
    }

    /// The `n`-th match of `pattern` from `from` (forward: starting after it; backward: before
    /// it), wrapping around with `'wrapscan'`.
    pub fn search_from<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        pattern: &str,
        forward: bool,
        from: usize,
        n: usize,
        ignore_case: Option<bool>,
    ) -> Option<usize> {
        let compiled = self.compile(pattern, ignore_case)?;
        let buf = Text::new(host);
        let lines = buf.line_count();
        let mut at = from;
        for _ in 0..n.max(1) {
            let line = buf.line_of(at);
            let col = at - buf.line_start(at);
            let mut found = None;
            for step in 0..=lines {
                let l = if forward {
                    let l = line + step;
                    if l >= lines {
                        if !self.settings.wrap_scan {
                            break;
                        }
                        l - lines
                    } else {
                        l
                    }
                } else if step > line {
                    if !self.settings.wrap_scan {
                        break;
                    }
                    lines + line - step
                } else {
                    line - step
                };
                let text = buf.line_text(l);
                let matches = compiled.matches(&text);
                let pick = if step == 0 {
                    if forward {
                        matches.iter().find(|m| m.range.start > col)
                    } else {
                        matches.iter().rev().find(|m| m.range.start < col)
                    }
                } else if step == lines {
                    // Back on the first line after wrapping: the part not yet searched.
                    if forward {
                        matches.iter().find(|m| m.range.start <= col)
                    } else {
                        matches.iter().rev().find(|m| m.range.start >= col)
                    }
                } else if forward {
                    matches.first()
                } else {
                    matches.last()
                };
                if let Some(m) = pick {
                    found = Some(buf.line_start_of(l) + m.range.start);
                    break;
                }
            }
            match found {
                Some(f) => at = f,
                None => {
                    self.error(format!("E486: Pattern not found: {pattern}"));
                    return None;
                }
            }
        }
        Some(at)
    }

    /// A search with its `/offset` (`e`, `s`, `b`, `+N`, `-N`).
    fn search_target<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        pattern: &str,
        forward: bool,
        offset: &str,
        n: usize,
        ignore_case: Option<bool>,
    ) -> Option<Target> {
        let from = host.cursor();
        let start = self.search_from(host, pattern, forward, from, n, ignore_case)?;
        let buf = Text::new(host);
        let (kind, rest) = match offset.chars().next() {
            Some(c @ ('e' | 's' | 'b')) => (Some(c), &offset[1..]),
            _ => (None, offset),
        };
        let amount: i64 = match rest {
            "" => 0,
            "+" => 1,
            "-" => -1,
            r => r.parse().unwrap_or(0),
        };
        match kind {
            None if offset.is_empty() => Some(Target {
                to: start,
                kind: Kind::Exclusive,
            }),
            None => {
                let line =
                    (buf.line_of(start) as i64 + amount).clamp(0, buf.line_count() as i64 - 1);
                Some(Target {
                    to: buf.line_start_of(line as usize),
                    kind: Kind::Linewise,
                })
            }
            Some('e') => {
                let compiled = self.compile(pattern, ignore_case)?;
                let ls = buf.line_start(start);
                let text = buf.line_text(buf.line_of(start));
                let end = compiled
                    .find_at(&text, start - ls)
                    .map_or(start, |m| ls + m.range.end);
                let last = if end > start { buf.prev(end) } else { start };
                let to = step_chars(&buf, last, amount);
                Some(Target {
                    to,
                    kind: Kind::Inclusive,
                })
            }
            Some(_) => Some(Target {
                to: step_chars(&buf, start, amount),
                kind: Kind::Exclusive,
            }),
        }
    }

    // ---- simple commands -------------------------------------------------------------------------

    pub fn simple<H: VimHost>(
        &mut self,
        host: &mut H,
        simple: Simple,
        reg: Option<char>,
        count: Option<usize>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let n = count.unwrap_or(1).max(1);
        match simple {
            Simple::Replace(c) => self.replace_chars(host, n, c),
            Simple::ToggleCase => self.toggle_case(host, n),
            Simple::Insert(at) => self.insert_at(host, at, n, opts),
            Simple::Put {
                before,
                cursor_after,
            } => self.put(host, reg, before, cursor_after, n, opts, clip),
            Simple::Join { spaces } => {
                if self.mode.is_visual() {
                    let (top, bottom) = self.visual_lines(host);
                    self.leave_visual(host);
                    self.join(host, top, bottom.max(top + 1), spaces);
                    return;
                }
                let line = Text::new(&*host).line_of(host.cursor());
                let last = Text::new(&*host).line_count() - 1;
                if line >= last {
                    self.fail();
                    return;
                }
                let count = count.unwrap_or(2).max(2);
                self.join(host, line, (line + count - 1).min(last), spaces);
            }
            Simple::Undo => {
                for i in 0..n {
                    if !host.undo() {
                        if i == 0 {
                            self.error("Already at oldest change");
                            self.fail();
                        }
                        break;
                    }
                }
                self.want_col = None;
            }
            Simple::Redo => {
                for i in 0..n {
                    if !host.redo() {
                        if i == 0 {
                            self.error("Already at newest change");
                            self.fail();
                        }
                        break;
                    }
                }
                self.want_col = None;
            }
            Simple::Visual(mode) => self.visual_key(host, mode),
            Simple::Reselect => self.reselect(host),
            Simple::SetMark(name) => {
                let at = Self::pos(host, host.cursor());
                self.marks.set(name, at);
            }
            Simple::Record(name) => self.start_recording(name),
            Simple::StopRecord => self.stop_recording(),
            Simple::Play(name) => self.play(host, name, count, opts, clip),
            Simple::Repeat => self.repeat(host, count, opts, clip),
            Simple::RepeatSubstitute => {
                // Vim runs `&` as the `:s` line.
                self.regs.last_ex = Some("s".into());
                let command = crate::ex::Substitute {
                    pattern: String::new(),
                    replacement: None,
                    global: false,
                    ignore_case: None,
                    count_only: false,
                    quiet: false,
                    keep_flags: false,
                    count: None,
                };
                if let Err(e) = self.substitute(host, None, command) {
                    self.error(e.0);
                    self.fail();
                }
            }
            Simple::Jump(step) => {
                let at = Self::pos(host, host.cursor());
                let mut target = None;
                for _ in 0..n {
                    match self.marks.step(at, step) {
                        Some(p) => target = Some(p),
                        None => break,
                    }
                }
                match target {
                    Some(p) => {
                        let to = Self::offset(host, p);
                        host.set_cursor(Text::new(&*host).normal_clamp(to));
                    }
                    None => self.fail(),
                }
            }
            Simple::Escape => self.leave_visual(host),
            Simple::Scroll(how) => self.scroll(host, how, count),
            Simple::Increment { add, progressive } => {
                if self.mode.is_visual() {
                    self.visual_increment(host, add, progressive, n);
                } else {
                    self.increment(host, add, n);
                }
            }
            Simple::Redraw => {}
            Simple::SwapEnds => {
                let cursor = host.cursor();
                host.set_cursor(self.anchor);
                self.anchor = cursor;
                if self.want_col == Some(usize::MAX) {
                    self.want_col = None;
                }
            }
            Simple::SwapCorner => {
                if self.mode != Mode::VisualBlock {
                    let cursor = host.cursor();
                    host.set_cursor(self.anchor);
                    self.anchor = cursor;
                    return;
                }
                let buf = self.text(&*host);
                let (a, c) = (self.anchor, host.cursor());
                let (al, ac) = (buf.line_of(a), buf.column(a));
                let (cl, cc) = (buf.line_of(c), buf.column(c));
                let anchor = buf.at_column(buf.line_start_of(al), cc);
                let cursor = buf.at_column(buf.line_start_of(cl), ac);
                self.anchor = anchor;
                host.set_cursor(cursor);
            }
            Simple::VisualInsert { append } => self.visual_insert(host, append),
            Simple::VisualReplace(c) => self.visual_replace(host, c),
            Simple::VisualPut { before } => self.visual_put(host, reg, before, n, opts, clip),
        }
    }

    fn insert_at<H: VimHost>(&mut self, host: &mut H, at: InsertAt, count: usize, opts: Options) {
        let cursor = host.cursor();
        let buf = Text::new(&*host);
        match at {
            InsertAt::Before => {}
            InsertAt::After => {
                if cursor < buf.line_end(cursor) {
                    let to = buf.next(cursor);
                    host.set_cursor(to);
                }
            }
            InsertAt::FirstNonBlank => {
                let to = buf.first_nonblank(buf.line_start(cursor));
                let to = if matches!(buf.at(to), Some(' ' | '\t')) {
                    buf.line_end(to)
                } else {
                    to
                };
                host.set_cursor(to);
            }
            InsertAt::LineEnd => {
                let to = buf.line_end(cursor);
                host.set_cursor(to);
            }
            InsertAt::LineStart => {
                let to = buf.line_start(cursor);
                host.set_cursor(to);
            }
            InsertAt::Below | InsertAt::Above => {
                if opts.single_line {
                    let to = if at == InsertAt::Below { buf.len() } else { 0 };
                    host.set_cursor(to);
                } else {
                    let (le, ls) = (buf.line_end(cursor), buf.line_start(cursor));
                    host.begin_group();
                    let tx = if at == InsertAt::Below {
                        Transaction::insert(le, "\n", le + 1)
                    } else {
                        Transaction::insert(ls, "\n", ls)
                    };
                    self.apply(host, tx);
                }
                self.enter_insert(host, count, Repeat::Lines, None);
                return;
            }
        }
        self.enter_insert(host, count, Repeat::Inline, None);
    }

    fn replace_chars<H: VimHost>(&mut self, host: &mut H, count: usize, c: char) {
        let from = host.cursor();
        let buf = Text::new(&*host);
        let le = buf.line_end(from);
        // The `count` characters must all be on the line.
        let end = buf.advance(from, count, le);
        if host.slice(from..end).chars().count() != count {
            self.fail();
            return;
        }
        self.changed = true;
        host.begin_group();
        // What `r` typed is the last inserted text (`".`).
        self.regs.last_insert = Some(c.to_string());
        if c == '\n' {
            self.apply(host, Transaction::replace(from..end, "\n", from + 1));
            return;
        }
        let with: String = std::iter::repeat_n(c, count).collect();
        let cursor = from + with.len() - c.len_utf8();
        self.apply(host, Transaction::replace(from..end, with, cursor));
        self.marks.last_change = Some(Self::pos(host, cursor));
    }

    fn toggle_case<H: VimHost>(&mut self, host: &mut H, count: usize) {
        let from = host.cursor();
        let buf = Text::new(&*host);
        let end = buf.advance(from, count, buf.line_end(from));
        if end == from {
            self.fail();
            return;
        }
        self.changed = true;
        host.begin_group();
        let old = host.slice(from..end).into_owned();
        let chars = old.chars().count();
        self.apply(
            host,
            Transaction::replace(from..end, toggle_case_of(&old), from),
        );
        let after = Text::new(&*host);
        let to = after.advance(from, chars, after.last_in_line(from).max(from));
        host.set_cursor(to);
    }

    // ---- Visual ----------------------------------------------------------------------------------

    /// `v`, `V`, `⌃V`: start it, switch to it, or (the mode it is in) leave it.
    fn visual_key<H: VimHost>(&mut self, host: &mut H, mode: Mode) {
        if self.mode == mode {
            self.leave_visual(host);
        } else if self.mode.is_visual() {
            self.mode = mode;
        } else {
            self.anchor = host.cursor();
            self.mode = mode;
            self.want_col = None;
        }
    }

    pub fn remember_visual<H: VimHost + ?Sized>(&mut self, host: &H) {
        if !self.mode.is_visual() {
            return;
        }
        let a = Self::pos(host, self.anchor);
        let c = Self::pos(host, host.cursor());
        self.last_visual = Some(VisualMemory {
            mode: self.mode,
            anchor: a,
            cursor: c,
            want_col: self.want_col,
        });
        let (start, end) = (a.min(c), a.max(c));
        if self.mode == Mode::VisualBlock {
            let buf = self.text(host);
            let block = self.block(host);
            let col = |line: usize, vcol: usize| {
                let ls = buf.line_start_of(line);
                buf.at_vcol(ls, vcol) - ls
            };
            self.marks.visual_start = Some(Pos {
                line: block.top,
                col: col(block.top, block.left),
            });
            self.marks.visual_end = Some(Pos {
                line: block.bottom,
                col: col(block.bottom, block.right.min(usize::MAX - 1)),
            });
        } else {
            self.marks.visual_start = Some(start);
            self.marks.visual_end = Some(end);
        }
    }

    pub fn leave_visual<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        self.remember_visual(host);
        self.mode = Mode::Normal;
        self.pending.clear();
        self.want_col = None;
        self.clamp_normal(host);
    }

    /// `gv`: the last selection again (in Visual mode, the two swap).
    fn reselect<H: VimHost>(&mut self, host: &mut H) {
        let Some(last) = self.last_visual else {
            self.fail();
            return;
        };
        if self.mode.is_visual() {
            self.remember_visual(host);
        }
        self.mode = last.mode;
        self.anchor = Self::offset(host, last.anchor);
        self.want_col = last.want_col;
        let to = Self::offset(host, last.cursor);
        host.set_cursor(to);
    }

    /// The first and last line of the selection.
    pub fn visual_lines<H: VimHost + ?Sized>(&self, host: &H) -> (usize, usize) {
        let buf = Text::new(host);
        let (a, c) = (buf.line_of(self.anchor), buf.line_of(host.cursor()));
        (a.min(c), a.max(c))
    }

    pub fn block<H: VimHost + ?Sized>(&self, host: &H) -> Block {
        let buf = self.text(host);
        let a = self.anchor.min(buf.len());
        let c = host.cursor();
        let (al, ac) = (buf.line_of(a), buf.column(a));
        let (cl, cc) = (buf.line_of(c), buf.column(c));
        // A wide character or a tab at the right edge is in the block whole.
        let right_edge = |i: usize, col: usize| buf.column(buf.next(i)).max(col + 1) - 1;
        let right = if buf.line_of(buf.next(a)) == al && buf.at(a) != Some('\n') {
            right_edge(a, ac)
        } else {
            ac
        };
        let right_c = if buf.at(c) != Some('\n') && buf.at(c).is_some() {
            right_edge(c, cc)
        } else {
            cc
        };
        Block {
            top: al.min(cl),
            bottom: al.max(cl),
            left: ac.min(cc),
            right: if self.want_col == Some(usize::MAX) {
                usize::MAX
            } else {
                right.max(right_c)
            },
        }
    }

    /// The charwise selection, end exclusive (`$` takes the line break).
    pub fn char_span<H: VimHost + ?Sized>(&self, host: &H) -> (usize, usize) {
        let buf = Text::new(host);
        let a = buf.boundary_up(self.anchor);
        let c = host.cursor();
        let (lo, hi) = (a.min(c), a.max(c));
        let mut end = buf.next(hi);
        if (self.want_col == Some(usize::MAX) && hi == c) || buf.at(hi) == Some('\n') {
            end = if buf.at(hi) == Some('\n') {
                hi + 1
            } else {
                let le = buf.line_end(hi);
                if le < buf.len() { le + 1 } else { le }
            };
        }
        (lo, end.max(lo))
    }

    pub fn visual_ranges<H: VimHost + ?Sized>(&self, host: &H) -> Vec<Range<usize>> {
        let buf = self.text(host);
        match self.mode {
            Mode::Visual => {
                let (lo, hi) = self.char_span(host);
                std::iter::once(lo..hi).collect()
            }
            Mode::VisualLine => {
                let (top, bottom) = self.visual_lines(host);
                let start = buf.line_start_of(top);
                let le = buf.line_end(buf.line_start_of(bottom));
                let end = if le < buf.len() { le + 1 } else { le };
                std::iter::once(start..end).collect()
            }
            Mode::VisualBlock => {
                let block = self.block(host);
                (block.top..=block.bottom)
                    .map(|line| {
                        block.row(&buf, line).unwrap_or_else(|| {
                            let le = buf.line_end(buf.line_start_of(line));
                            le..le
                        })
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// `iw`, `a(`, … in Visual mode.
    pub fn visual_object<H: VimHost>(
        &mut self,
        host: &mut H,
        object: Object,
        around: bool,
        count: Option<usize>,
    ) {
        let from = host.cursor();
        let single = self.anchor == from;
        let mut n = count.unwrap_or(1).max(1);
        let span = if let (Object::Block { open, close }, false) = (object, single) {
            // A selection already made: the block around it, larger each time.
            let (lo, hi) = self.char_span(host);
            let mut found = None;
            for level in 1..=n + 8 {
                let Some((s, e)) = Text::new(&*host).enclosing(lo, open, close, level) else {
                    break;
                };
                let (start, end) = if around {
                    (s, Text::new(&*host).next(e))
                } else {
                    (Text::new(&*host).next(s), e)
                };
                if start < lo || end > hi {
                    n -= 1;
                    if n == 0 {
                        found = Some((start, end));
                        break;
                    }
                }
            }
            found
        } else {
            self.object_range(host, object, around, n)
        };
        let Some((start, end)) = span else {
            self.fail();
            return;
        };
        if object == Object::Paragraph && self.mode == Mode::Visual {
            self.mode = Mode::VisualLine;
        }
        let buf = Text::new(&*host);
        if end > start {
            if single || start < self.anchor.min(from) {
                self.anchor = start;
            }
            let to = buf.prev(end);
            host.set_cursor(to);
        }
    }

    /// `I` / `A` in Visual mode: a block inserts on every line; otherwise before the first
    /// line or after the last.
    fn visual_insert<H: VimHost>(&mut self, host: &mut H, append: bool) {
        let mode = self.mode;
        let block = self.block(host);
        let (top, bottom) = self.visual_lines(host);
        self.leave_visual(host);
        let buf = self.text(&*host);
        if mode == Mode::VisualBlock {
            let lines: Vec<usize> = (block.top + 1..=block.bottom)
                .filter(|&l| append || buf.line_width(l) > block.left)
                .collect();
            let (col, at_end) = if append {
                if block.right == usize::MAX {
                    (0, true)
                } else {
                    (block.right + 1, false)
                }
            } else {
                (block.left, false)
            };
            let ls = buf.line_start_of(block.top);
            let le = buf.line_end(ls);
            let width = buf.line_width(block.top);
            let corner = buf.normal_clamp(buf.at_vcol(ls, block.left));
            let target = if at_end {
                Ok(le)
            } else if width >= col {
                Ok(buf.at_vcol(ls, col))
            } else {
                Err(" ".repeat(col - width))
            };
            host.begin_group();
            let at = match target {
                Ok(at) => at,
                Err(pad) => {
                    self.apply(host, Transaction::insert(le, pad.clone(), le + pad.len()));
                    le + pad.len()
                }
            };
            host.set_cursor(at);
            self.enter_insert(
                host,
                1,
                Repeat::Inline,
                Some(BlockInsert {
                    lines,
                    col,
                    at_end,
                    pad: append,
                    corner: Some(corner),
                }),
            );
            return;
        }
        let at = if append {
            buf.line_end(buf.line_start_of(bottom))
        } else {
            buf.line_start_of(top)
        };
        host.set_cursor(at);
        self.enter_insert(host, 1, Repeat::Inline, None);
    }

    /// `r` in Visual mode: every selected character (not line breaks) becomes `c`.
    fn visual_replace<H: VimHost>(&mut self, host: &mut H, c: char) {
        let mode = self.mode;
        let ranges: Vec<Range<usize>> = match mode {
            Mode::Visual => {
                let (lo, hi) = self.char_span(host);
                std::iter::once(lo..hi).collect()
            }
            Mode::VisualLine => {
                let (top, bottom) = self.visual_lines(host);
                let buf = Text::new(&*host);
                std::iter::once(buf.line_start_of(top)..buf.line_end(buf.line_start_of(bottom)))
                    .collect()
            }
            _ => {
                let block = self.block(host);
                let buf = Text::new(&*host);
                (block.top..=block.bottom)
                    .filter_map(|l| block.row(&buf, l))
                    .collect()
            }
        };
        let start = ranges.first().map_or(host.cursor(), |r| r.start);
        self.leave_visual(host);
        let mut edits = Vec::new();
        for range in ranges {
            let old = host.slice(range.clone()).into_owned();
            let new: String = old.chars().map(|o| if o == '\n' { o } else { c }).collect();
            if new != old {
                edits.push(crate::host::Edit { range, text: new });
            }
        }
        if edits.is_empty() {
            return;
        }
        host.begin_group();
        self.apply(
            host,
            Transaction {
                edits,
                cursor: start,
            },
        );
        self.close_group(host);
    }
}

/// `/pattern/offset`: the pattern (its delimiter escaped with `\`) and the offset.
pub(crate) fn split_offset(text: &str, delimiter: char) -> (String, String) {
    let mut pattern = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                pattern.push('\\');
                if let Some((_, n)) = chars.next() {
                    pattern.push(n);
                }
            }
            c if c == delimiter => return (pattern, text[at + c.len_utf8()..].to_owned()),
            c => pattern.push(c),
        }
    }
    (pattern, String::new())
}

/// `n` characters on (back when negative) from `at`, across lines.
fn step_chars<H: VimHost + ?Sized>(buf: &Text<'_, H>, at: usize, n: i64) -> usize {
    let mut at = at;
    if n >= 0 {
        for _ in 0..n {
            let next = buf.next(at);
            if next >= buf.len() {
                break;
            }
            at = if buf.at(next) == Some('\n') && buf.line_start(next) != next {
                next + 1
            } else {
                next
            };
        }
    } else {
        for _ in 0..(-n) {
            match buf.dec(at) {
                Some(p) => at = p,
                None => break,
            }
        }
    }
    at
}

pub(crate) fn toggle_case_of(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_lowercase() {
                c.to_uppercase().collect::<Vec<_>>()
            } else if c.is_uppercase() {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
