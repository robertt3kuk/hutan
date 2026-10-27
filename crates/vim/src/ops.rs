//! Operators (`d c y < > = g~ gu gU g?`) on motions, objects, lines, Visual selections and
//! blocks; puts, joins, shifts and the indenter.

use crate::command::{Motion, Object, Op, Target as OpTarget, Widen};
use crate::host::{Clipboard, Edit, Transaction, VimHost};
use crate::key::{Mode, Options};
use crate::marks::Pos;
use crate::normal::{Block, toggle_case_of};
use crate::register::{Register, RegisterKind, Write};
use crate::text::{Kind, Span, Text, class};
use crate::vim::{BlockInsert, Core, Repeat};

/// The text of an operator: a span, or a block.
#[derive(Debug, Clone, Copy)]
enum Region {
    Span(Span),
    Block(Block),
}

impl Core {
    // ---- registers ---------------------------------------------------------------------------

    /// Stores what an operator took: `reg` as given (`None` the unnamed one), the system
    /// clipboard for `"+` / `"*` and, with `use_system_clipboard`, for the unnamed one.
    fn store(
        &mut self,
        reg: Option<char>,
        register: Register,
        write: Write,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        match reg {
            Some('_') => {}
            Some('+' | '*') => {
                clip.write(&register.clipboard_form());
                self.regs.store(None, register, write);
            }
            Some('"') | None => {
                if opts.use_system_clipboard && !register.text.is_empty() {
                    clip.write(&register.clipboard_form());
                }
                self.regs.store(None, register, write);
            }
            Some(name) => self.regs.store(Some(name), register, write),
        }
    }

    /// What `p` puts: the register, or the system clipboard when it holds something else.
    fn paste_source(
        &mut self,
        reg: Option<char>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Option<Register> {
        match reg {
            Some('_') => None,
            Some('+' | '*') => clip
                .read()
                .filter(|t| !t.is_empty())
                .map(Register::from_clipboard),
            Some('"') | None => {
                let unnamed = self.regs.get('"');
                if opts.use_system_clipboard
                    && let Some(text) = clip.read().filter(|t| !t.is_empty())
                {
                    if unnamed.as_ref().is_some_and(|u| u.clipboard_form() == text) {
                        return unnamed;
                    }
                    return Some(Register::from_clipboard(text));
                }
                unnamed
            }
            Some(name) => self.regs.get(name),
        }
    }

    // ---- operators on motions and objects --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn operate<H: VimHost>(
        &mut self,
        host: &mut H,
        op: Op,
        target: &OpTarget,
        reg: Option<char>,
        count: Option<usize>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let from = host.cursor();
        let n = count.unwrap_or(1).max(1);
        let mut reg_one = false;
        // Where Vim's caret is when the change starts: where undo brings it back.
        let undo_at;
        let span = match target {
            OpTarget::Lines => {
                let buf = Text::new(&*host);
                let line = buf.line_of(from);
                let last = buf.line_count() - 1;
                if n > 1 && line >= last {
                    self.fail();
                    return;
                }
                undo_at = from.min(buf.first_nonblank(buf.line_start(from)));
                if opts.single_line {
                    Span {
                        start: 0,
                        end: buf.len(),
                        linewise: false,
                    }
                } else {
                    let end = (line + n - 1).min(last);
                    Span {
                        start: buf.line_start_of(line),
                        end: buf.line_end(buf.line_start_of(end)),
                        linewise: true,
                    }
                }
            }
            OpTarget::Object { object, around } => {
                let Some((start, end)) = self.object_range(host, *object, *around, n) else {
                    self.fail();
                    return;
                };
                let buf = Text::new(&*host);
                let mut span = Span {
                    start,
                    end,
                    linewise: false,
                };
                match object {
                    Object::Paragraph => {
                        span = Span {
                            start: buf.line_start(start),
                            end: buf.line_end(buf.prev(end).max(start)),
                            linewise: true,
                        };
                    }
                    Object::Block { .. } if !around => {
                        if start >= end {
                            // An empty block: the caret goes inside it, nothing changes.
                            host.set_cursor(buf.normal_clamp(start));
                            return;
                        }
                        span = exclusive_rule(&buf, span);
                    }
                    _ => {}
                }
                undo_at = span.start;
                span
            }
            OpTarget::Motion(motion) => {
                if !matches!(
                    motion,
                    Motion::Up | Motion::Down | Motion::ScreenUp | Motion::ScreenDown
                ) {
                    self.want_col = None;
                }
                let buf = Text::new(&*host);
                // `cw` on a word changes up to its end, like `ce` (but stays on a one-letter word).
                let on_word = buf
                    .at(from)
                    .is_some_and(|c| c != '\n' && class(c, false) != 0);
                let target = match (op, motion) {
                    (Op::Change, Motion::Word { big }) if on_word => {
                        let mut to = buf.run_end(from, *big);
                        for _ in 1..n {
                            to = buf.word_end(to, *big);
                        }
                        Some(crate::text::Target {
                            to,
                            kind: Kind::Inclusive,
                        })
                    }
                    _ => self.target(host, motion, count, true, opts),
                };
                let Some(target) = target else {
                    self.fail();
                    return;
                };
                if motion.is_jump() {
                    self.marks.jump_from(Self::pos(host, from));
                }
                let buf = Text::new(&*host);
                let mut span = buf.span(from, target, opts.single_line);
                if target.kind == Kind::Exclusive && !opts.single_line {
                    span = exclusive_rule(&buf, span);
                }
                undo_at = from.min(target.to);
                reg_one = motion.uses_register_one();
                span
            }
        };
        let mut span = span;
        if op == Op::Delete && !span.linewise && !opts.single_line {
            span = delete_linewise_rule(&Text::new(&*host), span);
        }
        let cursor_from = if matches!(target, OpTarget::Object { .. }) {
            span.start
        } else {
            from
        };
        if op != Op::Yank {
            host.set_cursor(undo_at);
        }
        self.apply_op(
            host,
            op,
            Region::Span(span),
            cursor_from,
            reg,
            reg_one,
            1,
            opts,
            clip,
        );
    }

    /// `iw`, `a(`, `ip`, … at the caret: `(start, end)`, end exclusive.
    pub fn object_range<H: VimHost + ?Sized>(
        &self,
        host: &H,
        object: Object,
        around: bool,
        n: usize,
    ) -> Option<(usize, usize)> {
        let buf = Text::new(host);
        let from = host.cursor();
        match object {
            Object::Word { big } => {
                let (start, mut end) = buf.word_object(from, around, big)?;
                for _ in 1..n {
                    if end >= buf.line_end(end) {
                        break;
                    }
                    let (_, e) = buf.word_object(end, around, big)?;
                    end = e;
                }
                Some((start, end))
            }
            Object::Sentence => buf.sentence_object(from, around, n),
            Object::Paragraph => {
                let (first, last) = buf.paragraph_object(buf.line_of(from), around, n)?;
                let start = buf.line_start_of(first);
                let end = buf.line_end(buf.line_start_of(last));
                Some((start, if end < buf.len() { end + 1 } else { end }))
            }
            Object::Block { open, close } => {
                let (s, e) = buf.enclosing(from, open, close, n)?;
                if around {
                    Some((s, buf.next(e)))
                } else {
                    Some(buf.inner_block(s, e))
                }
            }
            Object::Quote(q) => buf.quote_object(from, q, around, n),
        }
    }

    // ---- Visual operators ------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn visual_operate<H: VimHost>(
        &mut self,
        host: &mut H,
        op: Op,
        widen: Widen,
        reg: Option<char>,
        count: Option<usize>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let mode = self.mode;
        let buf = Text::new(&*host);
        let region = match (mode, widen) {
            (Mode::VisualBlock, Widen::Lines) if matches!(op, Op::Delete | Op::Change) => {
                let mut block = self.block(host);
                block.right = usize::MAX;
                Region::Block(block)
            }
            (Mode::VisualBlock, Widen::None) => Region::Block(self.block(host)),
            (Mode::VisualLine, _) | (_, Widen::Lines) => {
                let (top, bottom) = self.visual_lines(host);
                Region::Span(Span {
                    start: buf.line_start_of(top),
                    end: buf.line_end(buf.line_start_of(bottom)),
                    linewise: true,
                })
            }
            _ => {
                let (start, end) = self.char_span(host);
                Region::Span(Span {
                    start,
                    end,
                    linewise: false,
                })
            }
        };
        let from = match region {
            Region::Span(span) => span.start,
            Region::Block(b) => {
                let ls = buf.line_start_of(b.top);
                buf.advance(ls, b.left, buf.line_end(ls))
            }
        };
        self.leave_visual(host);
        let amount = count.unwrap_or(1).max(1);
        if let (Region::Block(_), Op::ShiftLeft | Op::ShiftRight) = (region, op) {
            self.error("Not supported: > and < on a Visual block");
            self.fail();
            return;
        }
        host.set_cursor(from);
        self.apply_op(host, op, region, from, reg, false, amount, opts, clip);
        // Changes made in Visual mode are not repeated by `.`.
        self.changed = op == Op::Change;
        self.rec.clear();
    }

    // ---- applying an operator --------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn apply_op<H: VimHost>(
        &mut self,
        host: &mut H,
        op: Op,
        region: Region,
        from: usize,
        reg: Option<char>,
        reg_one: bool,
        amount: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        if let Region::Block(block) = region {
            self.apply_block(host, op, block, reg, opts, clip);
            return;
        }
        let Region::Span(span) = region else {
            return;
        };
        let buf = Text::new(&*host);
        let (first_line, last_line) = (buf.line_of(span.start), buf.line_of(span.end));
        match op {
            Op::Delete | Op::Change | Op::Yank => {
                let text = host.slice(span.start..span.end).into_owned();
                let empty = span.start == span.end && !span.linewise;
                if !empty {
                    let register = Register {
                        kind: if span.linewise {
                            RegisterKind::Line
                        } else {
                            RegisterKind::Char
                        },
                        text,
                    };
                    let write = if op == Op::Yank {
                        Write::Yank
                    } else {
                        let lines = span.linewise || register.text.contains('\n');
                        Write::Delete {
                            one: reg_one || lines,
                            small: !lines,
                        }
                    };
                    self.store(reg, register, write, opts, clip);
                }
                self.marks.change_start = Some(Self::pos(host, span.start));
                let last = Text::new(&*host).prev(span.end).max(span.start);
                self.marks.change_end = Some(Self::pos(host, last));
                match op {
                    Op::Yank => {
                        let buf = self.text(&*host);
                        let at = if span.linewise {
                            buf.at_column(buf.line_start(span.start), buf.column(from))
                        } else {
                            span.start
                        };
                        host.set_cursor(at);
                    }
                    Op::Delete => {
                        if empty {
                            return;
                        }
                        self.changed = true;
                        host.begin_group();
                        self.delete_span(host, span, false);
                        self.marks.last_change = Some(Self::pos(host, host.cursor()));
                    }
                    _ => {
                        self.changed = true;
                        host.begin_group();
                        if !empty {
                            self.delete_span(host, span, true);
                        }
                        self.enter_insert(host, 1, Repeat::Inline, None);
                    }
                }
            }
            Op::ShiftLeft | Op::ShiftRight => {
                self.changed = true;
                host.begin_group();
                self.shift_lines(host, first_line, last_line, op == Op::ShiftLeft, amount);
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(first_line));
                host.set_cursor(to);
            }
            Op::Indent => {
                self.changed = true;
                host.begin_group();
                self.indent_lines(host, first_line, last_line);
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(first_line));
                host.set_cursor(to);
            }
            Op::Tilde | Op::Lower | Op::Upper | Op::Rot13 => {
                let old = host.slice(span.start..span.end).into_owned();
                let new = case_of(op, &old);
                let at = span.start;
                if new != old {
                    self.changed = true;
                    host.begin_group();
                    self.apply(host, Transaction::replace(span.start..span.end, new, at));
                } else {
                    host.set_cursor(at);
                }
            }
        }
    }

    /// Removes `span`. A deleted line takes its newline with it, unless `keep_line` (a changed
    /// line stays, empty).
    fn delete_span<H: VimHost>(&mut self, host: &mut H, span: Span, keep_line: bool) {
        if !span.linewise || keep_line {
            self.apply(host, Transaction::delete(span.start..span.end, span.start));
            return;
        }
        let buf = Text::new(&*host);
        let (start, end) = if span.end < buf.len() {
            (span.start, buf.next(span.end))
        } else if span.start > 0 {
            (buf.prev(span.start), span.end)
        } else {
            (span.start, span.end)
        };
        let line = buf.line_of(span.start);
        self.apply(host, Transaction::delete(start..end, start));
        let after = Text::new(&*host);
        let line = line.min(after.line_count() - 1);
        let at = after.first_nonblank(after.line_start_of(line));
        host.set_cursor(at);
    }

    fn apply_block<H: VimHost>(
        &mut self,
        host: &mut H,
        op: Op,
        block: Block,
        reg: Option<char>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let buf = self.text(&*host);
        let rows: Vec<(usize, Option<std::ops::Range<usize>>)> = (block.top..=block.bottom)
            .map(|l| (l, block.row(&buf, l)))
            .collect();
        let top_ls = buf.line_start_of(block.top);
        let corner = buf.at_vcol(top_ls, block.left);
        let width = block.width(&buf);
        // `c`: the text typed on the first line goes to every line that reaches the block.
        let reach: Vec<usize> = (block.top + 1..=block.bottom)
            .filter(|&l| buf.line_width(l) >= block.left)
            .collect();
        let texts: Vec<String> = rows
            .iter()
            .map(|(_, r)| {
                r.clone()
                    .map_or_else(String::new, |r| host.slice(r).into_owned())
            })
            .collect();
        match op {
            Op::Delete | Op::Change | Op::Yank => {
                let register = Register {
                    text: texts.join("\n"),
                    kind: RegisterKind::Block { width },
                };
                let write = if op == Op::Yank {
                    Write::Yank
                } else {
                    Write::Delete {
                        one: block.bottom > block.top,
                        small: block.bottom == block.top,
                    }
                };
                self.store(reg, register, write, opts, clip);
                if op == Op::Yank {
                    host.set_cursor(Text::new(&*host).normal_clamp(corner));
                    return;
                }
                let edits: Vec<Edit> = rows
                    .iter()
                    .filter_map(|(_, r)| r.clone())
                    .filter(|r| !r.is_empty())
                    .map(|range| Edit {
                        range,
                        text: String::new(),
                    })
                    .collect();
                self.changed = true;
                host.begin_group();
                if !edits.is_empty() {
                    self.apply(
                        host,
                        Transaction {
                            edits,
                            cursor: corner,
                        },
                    );
                }
                if op == Op::Delete {
                    host.set_cursor(Text::new(&*host).normal_clamp(corner));
                    return;
                }
                let buf = self.text(&*host);
                let ls = buf.line_start_of(block.top);
                let at = buf.at_vcol(ls, block.left);
                host.set_cursor(at);
                self.enter_insert(
                    host,
                    1,
                    Repeat::Inline,
                    Some(BlockInsert {
                        lines: reach,
                        col: block.left,
                        at_end: false,
                        pad: false,
                        corner: None,
                    }),
                );
            }
            Op::Tilde | Op::Lower | Op::Upper | Op::Rot13 => {
                let edits: Vec<Edit> = rows
                    .iter()
                    .zip(&texts)
                    .filter_map(|((_, r), old)| {
                        let r = r.clone()?;
                        let new = case_of(op, old);
                        (new != *old).then_some(Edit {
                            range: r,
                            text: new,
                        })
                    })
                    .collect();
                if !edits.is_empty() {
                    self.changed = true;
                    host.begin_group();
                    self.apply(
                        host,
                        Transaction {
                            edits,
                            cursor: corner,
                        },
                    );
                }
                host.set_cursor(Text::new(&*host).normal_clamp(corner));
            }
            Op::Indent => {
                self.changed = true;
                host.begin_group();
                self.indent_lines(host, block.top, block.bottom);
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(block.top));
                host.set_cursor(to);
            }
            Op::ShiftLeft | Op::ShiftRight => {}
        }
    }

    // ---- put -------------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn put<H: VimHost>(
        &mut self,
        host: &mut H,
        reg: Option<char>,
        before: bool,
        cursor_after: bool,
        count: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let Some(mut register) = self.paste_source(reg, opts, clip) else {
            self.error(format!("E353: Nothing in register {}", reg.unwrap_or('"')));
            self.fail();
            return;
        };
        if opts.single_line {
            register.kind = RegisterKind::Char;
            register.text = register.text.replace(['\r', '\n'], " ");
        }
        if register.text.is_empty() && register.kind == RegisterKind::Char {
            return;
        }
        self.changed = true;
        host.begin_group();
        let from = host.cursor();
        let buf = self.text(&*host);
        match register.kind {
            RegisterKind::Line => {
                let block = vec![register.text.as_str(); count].join("\n");
                let lines = block.matches('\n').count() + 1;
                let first_line = if before {
                    buf.line_of(from)
                } else {
                    buf.line_of(from) + 1
                };
                let tx = if before {
                    let ls = buf.line_start(from);
                    Transaction::insert(ls, format!("{block}\n"), ls)
                } else {
                    let le = buf.line_end(from);
                    Transaction::insert(le, format!("\n{block}"), le + 1)
                };
                self.apply(host, tx);
                let after = Text::new(&*host);
                self.marks.change_start = Some(Pos {
                    line: first_line,
                    col: 0,
                });
                self.marks.change_end = Some(Pos {
                    line: first_line + lines - 1,
                    col: 0,
                });
                let to = if cursor_after {
                    let next = first_line + lines;
                    if next < after.line_count() {
                        after.line_start_of(next)
                    } else {
                        after.line_start_of(after.line_count() - 1)
                    }
                } else {
                    after.first_nonblank(after.line_start_of(first_line))
                };
                host.set_cursor(to);
            }
            RegisterKind::Char => {
                let block = register.text.repeat(count);
                let at = if before || buf.at(from).is_none_or(|c| c == '\n') {
                    from
                } else {
                    buf.next(from)
                };
                let cursor = if cursor_after {
                    at + block.len()
                } else if block.contains('\n') {
                    at
                } else if let Some(last) = block.chars().next_back() {
                    at + block.len() - last.len_utf8()
                } else {
                    at
                };
                self.apply(host, Transaction::insert(at, block.clone(), cursor));
                self.marks.change_start = Some(Self::pos(host, at));
                let last = Text::new(&*host).prev(at + block.len()).max(at);
                self.marks.change_end = Some(Self::pos(host, last));
                if cursor_after {
                    host.set_cursor(cursor);
                }
            }
            RegisterKind::Block { width } => {
                let rows: Vec<&str> = register.text.split('\n').collect();
                let line = buf.line_of(from);
                let col = if !before && buf.at(from).is_some_and(|c| c != '\n') {
                    buf.column(buf.next(from))
                } else {
                    buf.column(from)
                };
                let mut edits = Vec::new();
                let mut tail = String::new();
                for (i, row) in rows.iter().enumerate() {
                    let row_width = buf.width_of_str(row);
                    let mut piece = String::new();
                    for k in 0..count {
                        piece.push_str(row);
                        if k + 1 < count {
                            piece.push_str(&" ".repeat(width.saturating_sub(row_width)));
                        }
                    }
                    let l = line + i;
                    if l < buf.line_count() {
                        let ls = buf.line_start_of(l);
                        let le = buf.line_end(ls);
                        let n = buf.line_width(l);
                        if n < col {
                            edits.push(Edit {
                                range: le..le,
                                text: format!("{}{piece}", " ".repeat(col - n)),
                            });
                        } else {
                            let at = buf.at_vcol(ls, col);
                            if at < le {
                                piece.push_str(&" ".repeat(width.saturating_sub(row_width)));
                            }
                            edits.push(Edit {
                                range: at..at,
                                text: piece,
                            });
                        }
                    } else {
                        tail.push('\n');
                        tail.push_str(&" ".repeat(col));
                        tail.push_str(&piece);
                    }
                }
                if !tail.is_empty() {
                    let len = buf.len();
                    match edits.last_mut() {
                        Some(last) if last.range.start == len => last.text.push_str(&tail),
                        _ => edits.push(Edit {
                            range: len..len,
                            text: tail,
                        }),
                    }
                }
                let ls = buf.line_start_of(line);
                let corner = buf.at_vcol(ls, col);
                self.apply(
                    host,
                    Transaction {
                        edits,
                        cursor: corner,
                    },
                );
                let after = self.text(&*host);
                let ls = after.line_start_of(line);
                let to = after.at_vcol(ls, col);
                host.set_cursor(after.normal_clamp(to));
            }
        }
    }

    /// `p` / `P` in Visual mode: the selection is replaced by the register (`p` puts what it
    /// replaced in the unnamed register).
    pub fn visual_put<H: VimHost>(
        &mut self,
        host: &mut H,
        reg: Option<char>,
        before: bool,
        count: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let Some(register) = self.paste_source(reg, opts, clip) else {
            self.leave_visual(host);
            self.error(format!("E353: Nothing in register {}", reg.unwrap_or('"')));
            self.fail();
            return;
        };
        let mode = self.mode;
        let linewise = mode == Mode::VisualLine;
        let buf = Text::new(&*host);
        let (start, end) = if linewise {
            let (top, bottom) = self.visual_lines(host);
            (
                buf.line_start_of(top),
                buf.line_end(buf.line_start_of(bottom)),
            )
        } else {
            self.char_span(host)
        };
        self.leave_visual(host);
        let old = host.slice(start..end).into_owned();
        let lines = linewise || old.contains('\n');
        let write = Write::Delete {
            one: lines,
            small: !lines,
        };
        let replaced = Register {
            text: old,
            kind: if linewise {
                RegisterKind::Line
            } else {
                RegisterKind::Char
            },
        };
        self.changed = true;
        host.begin_group();
        let text = match (linewise, register.kind) {
            (true, RegisterKind::Line) | (false, RegisterKind::Char) => {
                vec![register.text.as_str(); count].join(if linewise { "\n" } else { "" })
            }
            (true, _) => register.text.repeat(count),
            (false, _) => format!("\n{}\n", vec![register.text.as_str(); count].join("\n")),
        };
        let cursor = if linewise || text.contains('\n') {
            start + usize::from(text.starts_with('\n'))
        } else {
            start + text.len() - text.chars().next_back().map_or(0, char::len_utf8)
        };
        self.apply(host, Transaction::replace(start..end, text, cursor));
        if linewise || register.kind == RegisterKind::Line {
            let buf = Text::new(&*host);
            let to = buf.first_nonblank(buf.line_start(cursor));
            host.set_cursor(to);
        }
        if !before {
            self.store(
                reg.filter(|r| !r.is_ascii_alphabetic()),
                replaced,
                write,
                opts,
                clip,
            );
        }
    }

    // ---- join --------------------------------------------------------------------------------------

    /// `J` (`spaces`) and `gJ` on lines `first..=last`.
    pub fn join<H: VimHost>(&mut self, host: &mut H, first: usize, last: usize, spaces: bool) {
        let buf = Text::new(&*host);
        let last = last.min(buf.line_count() - 1);
        if last <= first {
            self.fail();
            return;
        }
        let first_text = buf.line_text(first);
        let mut sum = first_text.len();
        let mut end1 = first_text.chars().next_back();
        let mut end2 = first_text.chars().rev().nth(1);
        let mut edits = Vec::new();
        let mut cursor_col = 0;
        for line in first + 1..=last {
            let ls = buf.line_start_of(line);
            let text = buf.line_text(line);
            let skipped = if spaces {
                text.len() - text.trim_start_matches([' ', '\t']).len()
            } else {
                0
            };
            let curr = &text[skipped..];
            let mut n = 0;
            if spaces
                && !curr.is_empty()
                && !curr.starts_with(')')
                && sum != 0
                && end1 != Some('\t')
            {
                let mut e1 = end1;
                if e1 == Some(' ') {
                    e1 = end2;
                } else {
                    n += 1;
                }
                if self.settings.join_spaces && matches!(e1, Some('.' | '?' | '!')) {
                    n += 1;
                }
            }
            edits.push(Edit {
                range: ls - 1..ls + skipped,
                text: " ".repeat(n),
            });
            cursor_col = sum;
            sum += n + curr.len();
            end1 = None;
            end2 = None;
            if spaces && !curr.is_empty() {
                end1 = curr.chars().next_back();
                end2 = curr.chars().rev().nth(1);
            }
        }
        let ls = buf.line_start_of(first);
        self.changed = true;
        host.begin_group();
        self.apply(host, Transaction { edits, cursor: ls });
        let after = Text::new(&*host);
        let le = after.line_end(ls);
        let to = after.boundary_up((ls + cursor_col).min(le));
        host.set_cursor(after.normal_clamp(to));
        self.marks.last_change = Some(Self::pos(host, host.cursor()));
    }

    // ---- shift and indent --------------------------------------------------------------------------

    /// The width of a line's indent (tabs to `'tabstop'`) and its length in bytes.
    pub(crate) fn indent_of(&self, text: &str) -> (usize, usize) {
        let ts = self.settings.tab_stop.max(1);
        let mut width = 0;
        let mut bytes = 0;
        for c in text.chars() {
            match c {
                ' ' => width += 1,
                '\t' => width += ts - width % ts,
                _ => break,
            }
            bytes += 1;
        }
        (width, bytes)
    }

    /// An indent `width` wide: tabs and spaces, or spaces with `'expandtab'`.
    pub(crate) fn indent_text(&self, width: usize) -> String {
        if self.settings.expand_tab {
            return " ".repeat(width);
        }
        let ts = self.settings.tab_stop.max(1);
        format!("{}{}", "\t".repeat(width / ts), " ".repeat(width % ts))
    }

    /// `>` / `<` on lines `first..=last`, `amount` times `'shiftwidth'`. Empty lines stay.
    pub fn shift_lines<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        first: usize,
        last: usize,
        left: bool,
        amount: usize,
    ) {
        let buf = Text::new(&*host);
        let sw = self.settings.shift();
        let mut edits = Vec::new();
        for line in first..=last.min(buf.line_count() - 1) {
            let text = buf.line_text(line);
            if text.is_empty() {
                continue;
            }
            let (width, bytes) = self.indent_of(&text);
            let new = if self.settings.shift_round {
                let rem = width % sw;
                if left {
                    if rem != 0 {
                        (width - rem).saturating_sub(sw * (amount - 1))
                    } else {
                        width.saturating_sub(sw * amount)
                    }
                } else {
                    width - rem + sw * amount
                }
            } else if left {
                width.saturating_sub(sw * amount)
            } else {
                width + sw * amount
            };
            let indent = self.indent_text(new);
            if indent != text[..bytes] {
                let ls = buf.line_start_of(line);
                edits.push(Edit {
                    range: ls..ls + bytes,
                    text: indent,
                });
            }
        }
        if edits.is_empty() {
            return;
        }
        let cursor = host.cursor();
        let cursor = edits.first().map_or(cursor, |e| e.range.start);
        self.apply(host, Transaction { edits, cursor });
    }

    /// `=`: each line indented from the line above it (one `'shiftwidth'` more after a line
    /// ending in `{`, one less for a line starting with `}`); blank lines lose their blanks.
    pub fn indent_lines<H: VimHost + ?Sized>(&mut self, host: &mut H, first: usize, last: usize) {
        let buf = Text::new(&*host);
        let sw = self.settings.shift();
        let last = last.min(buf.line_count() - 1);
        // The line above the range sets the start.
        let mut prev: Option<(usize, bool)> = None;
        let mut l = first;
        while l > 0 {
            l -= 1;
            let text = buf.line_text(l);
            if !text.trim().is_empty() {
                let (width, _) = self.indent_of(&text);
                prev = Some((width, text.trim_end().ends_with('{')));
                break;
            }
        }
        let mut edits = Vec::new();
        for line in first..=last {
            let text = buf.line_text(line);
            let (_, bytes) = self.indent_of(&text);
            let body = &text[bytes..];
            let ls = buf.line_start_of(line);
            if body.is_empty() {
                if bytes > 0 {
                    edits.push(Edit {
                        range: ls..ls + bytes,
                        text: String::new(),
                    });
                }
                continue;
            }
            let mut width = match prev {
                Some((w, true)) => w + sw,
                Some((w, false)) => w,
                None => 0,
            };
            if body.starts_with('}') {
                width = width.saturating_sub(sw);
            }
            let indent = self.indent_text(width);
            if indent != text[..bytes] {
                edits.push(Edit {
                    range: ls..ls + bytes,
                    text: indent,
                });
            }
            prev = Some((width, body.trim_end().ends_with('{')));
        }
        if edits.is_empty() {
            return;
        }
        let cursor = host.cursor();
        let cursor = edits.first().map_or(cursor, |e| e.range.start);
        self.apply(host, Transaction { edits, cursor });
    }
}

/// Whether only blanks precede `at` on its line.
fn in_indent<H: VimHost + ?Sized>(buf: &Text<'_, H>, at: usize) -> bool {
    let mut i = buf.line_start(at);
    while i < at {
        if !matches!(buf.at(i), Some(' ' | '\t')) {
            return false;
        }
        i += 1;
    }
    true
}

/// A charwise delete over several lines that starts in the indent and leaves only blanks after
/// its end deletes the lines instead (Vim's `d}` rule in `:help d`).
fn delete_linewise_rule<H: VimHost + ?Sized>(buf: &Text<'_, H>, span: Span) -> Span {
    if span.end <= span.start {
        return span;
    }
    let (first, last) = (buf.line_of(span.start), buf.line_of(buf.prev(span.end)));
    if first == last || !in_indent(buf, span.start) {
        return span;
    }
    let le = buf.line_end(span.end);
    let mut i = span.end;
    while i < le {
        if !matches!(buf.at(i), Some(' ' | '\t')) {
            return span;
        }
        i += 1;
    }
    Span {
        start: buf.line_start_of(first),
        end: buf.line_end(buf.line_start_of(last)),
        linewise: true,
    }
}

/// Vim's rule for an exclusive motion that ends in the first column of a later line
/// (`:help exclusive-linewise`): the end moves to the end of the line before; and when the start
/// is at or before the first non-blank of its line, the motion becomes linewise.
fn exclusive_rule<H: VimHost + ?Sized>(buf: &Text<'_, H>, span: Span) -> Span {
    if span.linewise || span.end <= span.start {
        return span;
    }
    let end_line = buf.line_of(span.end);
    if buf.line_start(span.end) != span.end || end_line <= buf.line_of(span.start) {
        return span;
    }
    let ls = buf.line_start(span.start);
    let mut only_blanks = true;
    let mut i = ls;
    while i < span.start {
        if !matches!(buf.at(i), Some(' ' | '\t')) {
            only_blanks = false;
            break;
        }
        i += 1;
    }
    let new_end = span.end - 1;
    if only_blanks {
        Span {
            start: ls,
            end: buf.line_end(new_end),
            linewise: true,
        }
    } else {
        Span {
            start: span.start,
            end: new_end,
            linewise: false,
        }
    }
}

fn rot13(c: char) -> char {
    match c {
        'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
        'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
        c => c,
    }
}

fn case_of(op: Op, text: &str) -> String {
    match op {
        Op::Tilde => toggle_case_of(text),
        Op::Lower => text.to_lowercase(),
        Op::Upper => text.to_uppercase(),
        Op::Rot13 => text.chars().map(rot13).collect(),
        _ => text.to_owned(),
    }
}
