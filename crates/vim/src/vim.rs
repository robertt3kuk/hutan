//! The modal state machine: [`Vim`] turns [`Key`]s into motions, operators and edits on a
//! [`VimHost`].

use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;

use crate::host::{Clipboard, Transaction, VimHost};
use crate::key::{Key, Mode, Options, Outcome};
use crate::text::{Find, Kind, Span, Target, Text, class};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Prefix {
    #[default]
    None,
    /// `g` was typed (`gg`).
    G,
    /// `r` was typed: the next character replaces.
    Replace,
    /// `f`, `t`, `F` or `T` was typed: the next character is the target.
    Find(Find),
    /// `i` or `a` after an operator (or in Visual): the next key names the object.
    Object { around: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    Word {
        big: bool,
    },
    Back {
        big: bool,
    },
    End {
        big: bool,
    },
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    Find(Find, char),
    RepeatFind {
        reverse: bool,
    },
    /// The caret's line and the ones below it (`dd`, `yy`, `cc` with a count).
    Lines,
}

/// What a command in progress has collected.
#[derive(Debug, Clone, Default)]
struct Pending {
    count: Option<usize>,
    op: Option<Op>,
    op_count: Option<usize>,
    prefix: Prefix,
}

impl Pending {
    fn is_idle(&self) -> bool {
        self.count.is_none()
            && self.op.is_none()
            && self.op_count.is_none()
            && self.prefix == Prefix::None
    }

    /// The count before and after the operator, multiplied; `None` when there was none.
    fn total(&self) -> Option<usize> {
        match (self.count, self.op_count) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        }
    }
}

/// The unnamed register.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Register {
    text: String,
    linewise: bool,
}

impl Register {
    /// What the system clipboard gets: a line goes with its newline.
    fn clipboard_form(&self) -> String {
        if self.linewise {
            format!("{}\n", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// The last change, for `.`: the keys of its command and the text typed in the Insert mode it
/// ended in.
#[derive(Debug, Clone, Default)]
struct Change {
    keys: Vec<Key>,
    inserted: Option<String>,
}

/// The modal state of one field. The text stays in the [`VimHost`] the keys are applied to.
///
/// `H` ties the state to the kind of host it works on (a later host can then keep
/// host-specific positions in it); a `Vim` holds no host, every call takes one.
pub struct Vim<H: VimHost> {
    mode: Mode,
    pending: Pending,
    /// Where Visual started (byte offset).
    anchor: usize,
    /// The column `j`/`k` aim for, in characters (`usize::MAX` after `$`).
    want_col: Option<usize>,
    register: Register,
    last_find: Option<(Find, char)>,
    /// Keys of the Normal-mode command in progress.
    rec: Vec<Key>,
    /// The command in progress changed the text.
    changed: bool,
    last_change: Option<Change>,
    replaying: bool,
    /// Where the current Insert session started (byte offset) and how much text followed the
    /// caret then: what was typed is between the start and the caret when it ends.
    insert_origin: Option<(usize, usize)>,
    host: PhantomData<fn(&mut H)>,
}

impl<H: VimHost> Default for Vim<H> {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            pending: Pending::default(),
            anchor: 0,
            want_col: None,
            register: Register::default(),
            last_find: None,
            rec: Vec::new(),
            changed: false,
            last_change: None,
            replaying: false,
            insert_origin: None,
            host: PhantomData,
        }
    }
}

impl<H: VimHost> Clone for Vim<H> {
    fn clone(&self) -> Self {
        Self {
            mode: self.mode,
            pending: self.pending.clone(),
            anchor: self.anchor,
            want_col: self.want_col,
            register: self.register.clone(),
            last_find: self.last_find,
            rec: self.rec.clone(),
            changed: self.changed,
            last_change: self.last_change.clone(),
            replaying: self.replaying,
            insert_origin: self.insert_origin,
            host: PhantomData,
        }
    }
}

impl<H: VimHost> fmt::Debug for Vim<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vim")
            .field("mode", &self.mode)
            .field("pending", &self.pending)
            .field("anchor", &self.anchor)
            .field("want_col", &self.want_col)
            .field("register", &self.register)
            .field("last_find", &self.last_find)
            .field("rec", &self.rec)
            .field("changed", &self.changed)
            .field("last_change", &self.last_change)
            .field("replaying", &self.replaying)
            .field("insert_origin", &self.insert_origin)
            .finish()
    }
}

impl<H: VimHost> Vim<H> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Back to Insert with nothing pending (a field that opens for typing). The register and
    /// the last change survive.
    pub fn reset(&mut self) {
        self.mode = Mode::Insert;
        self.pending = Pending::default();
        self.rec.clear();
        self.changed = false;
        self.want_col = None;
        self.insert_origin = None;
    }

    /// The keys typed so far of a command in progress (`2d`), for display.
    pub fn pending_keys(&self) -> String {
        let mut out = String::new();
        if let Some(count) = self.pending.count {
            out.push_str(&count.to_string());
        }
        match self.pending.op {
            Some(Op::Delete) => out.push('d'),
            Some(Op::Change) => out.push('c'),
            Some(Op::Yank) => out.push('y'),
            None => {}
        }
        if let Some(count) = self.pending.op_count {
            out.push_str(&count.to_string());
        }
        match self.pending.prefix {
            Prefix::None => {}
            Prefix::G => out.push('g'),
            Prefix::Replace => out.push('r'),
            Prefix::Find(f) => out.push(match (f.forward, f.till) {
                (true, false) => 'f',
                (true, true) => 't',
                (false, false) => 'F',
                (false, true) => 'T',
            }),
            Prefix::Object { around } => out.push(if around { 'a' } else { 'i' }),
        }
        out
    }

    /// What the unnamed register holds, and whether it is whole lines.
    pub fn register(&self) -> (&str, bool) {
        (&self.register.text, self.register.linewise)
    }

    /// The selected bytes in Visual mode, the character under the caret included.
    pub fn selection(&self, host: &H) -> Option<Range<usize>> {
        if self.mode != Mode::Visual {
            return None;
        }
        let text = Text::new(host);
        let len = text.len();
        let cursor = host.cursor();
        let (lo, hi) = (self.anchor.min(cursor), self.anchor.max(cursor));
        let hi = text.at(hi.min(len)).map_or(len, |c| hi + c.len_utf8());
        Some(lo.min(len)..hi.min(len))
    }

    /// Applies a key to `host`.
    pub fn key(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        match self.mode {
            Mode::Insert => self.insert_key(host, key, opts),
            Mode::Normal | Mode::Visual => self.command_key(host, key, opts, clip),
        }
    }

    // ---- Insert ----------------------------------------------------------------------------

    fn insert_key(&mut self, host: &mut H, key: Key, opts: Options) -> Outcome {
        if key == Key::Escape {
            self.leave_insert(host);
            return Outcome::Handled;
        }
        if opts.insert_passthrough {
            return Outcome::Ignored;
        }
        match key {
            Key::Char(c) => host.type_text(c.encode_utf8(&mut [0; 4])),
            Key::Backspace => host.backspace(),
            Key::Delete => host.delete_forward(),
            Key::Left => host.move_left(),
            Key::Right => host.move_right(),
            Key::Home => host.move_line_start(),
            Key::End => host.move_line_end(),
            Key::Up if !opts.single_line => {
                host.move_up();
            }
            Key::Down if !opts.single_line => {
                host.move_down();
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Handled
    }

    fn enter_insert(&mut self, host: &mut H) {
        host.begin_group();
        self.mode = Mode::Insert;
        let cursor = host.cursor();
        self.insert_origin = Some((cursor, host.len() - cursor));
        self.pending = Pending::default();
    }

    fn leave_insert(&mut self, host: &mut H) {
        if !self.rec.is_empty() && !self.replaying {
            let inserted = self.insert_origin.and_then(|(start, tail)| {
                let cursor = host.cursor();
                (start <= cursor
                    && host.len() - cursor == tail
                    && host.char_at(start).is_some()
                    && start < cursor)
                    .then(|| host.slice(start..cursor).into_owned())
            });
            self.last_change = Some(Change {
                keys: std::mem::take(&mut self.rec),
                inserted,
            });
        }
        self.rec.clear();
        self.changed = false;
        self.insert_origin = None;
        host.end_group();
        self.mode = Mode::Normal;
        self.want_col = None;
        // Like Vim, the caret steps back onto the last character typed.
        let buf = Text::new(&*host);
        let at = host.cursor();
        if at > buf.line_start(at) {
            let to = buf.prev(at);
            host.set_cursor(to);
        }
    }

    // ---- Normal and Visual -----------------------------------------------------------------

    fn command_key(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let outcome = self.dispatch(host, key, opts, clip);
        if self.mode == Mode::Normal && self.pending.is_idle() {
            // The command is complete.
            if self.changed && !self.replaying {
                self.last_change = Some(Change {
                    keys: std::mem::take(&mut self.rec),
                    inserted: None,
                });
            }
            self.rec.clear();
            self.changed = false;
            host.end_group();
            self.clamp_normal(host);
        } else if self.mode == Mode::Visual {
            self.rec.clear();
            self.changed = false;
        }
        outcome
    }

    fn dispatch(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let visual = self.mode == Mode::Visual;
        // `.` replays; it is not itself a change to replay.
        if !visual && self.pending.is_idle() && key == Key::Char('.') {
            self.repeat(host, opts, clip);
            return Outcome::Handled;
        }
        if !visual && !self.replaying {
            self.rec.push(key);
        }

        match self.pending.prefix {
            Prefix::None => {}
            prefix => return self.dispatch_prefixed(host, prefix, key, opts, clip),
        }

        match key {
            Key::Escape => {
                if !self.pending.is_idle() {
                    self.pending = Pending::default();
                    Outcome::Handled
                } else if visual {
                    self.leave_visual(host);
                    Outcome::Handled
                } else {
                    self.rec.clear();
                    Outcome::Ignored
                }
            }
            Key::Enter | Key::Tab => {
                self.pending = Pending::default();
                Outcome::Ignored
            }
            Key::Ctrl('r') if !visual => {
                self.pending = Pending::default();
                host.redo();
                self.want_col = None;
                Outcome::Handled
            }
            Key::Ctrl(_) => {
                self.pending = Pending::default();
                Outcome::Ignored
            }
            Key::Left => self.motion_key(host, Motion::Left, opts, clip),
            Key::Right => self.motion_key(host, Motion::Right, opts, clip),
            Key::Up => self.motion_key(host, Motion::Up, opts, clip),
            Key::Down => self.motion_key(host, Motion::Down, opts, clip),
            Key::Home => self.motion_key(host, Motion::LineStart, opts, clip),
            Key::End => self.motion_key(host, Motion::LineEnd, opts, clip),
            Key::Backspace => self.motion_key(host, Motion::Left, opts, clip),
            Key::Delete => self.char_command(host, 'x', opts, clip),
            Key::Char(c) => self.char_command(host, c, opts, clip),
        }
    }

    fn dispatch_prefixed(
        &mut self,
        host: &mut H,
        prefix: Prefix,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.pending.prefix = Prefix::None;
        let Key::Char(c) = key else {
            self.pending = Pending::default();
            return Outcome::Handled;
        };
        match prefix {
            Prefix::None => {}
            Prefix::G => {
                if c == 'g' {
                    self.motion_key(host, Motion::FileStart, opts, clip);
                } else {
                    self.pending = Pending::default();
                }
            }
            Prefix::Replace => {
                let count = self.pending.total().unwrap_or(1);
                self.pending = Pending::default();
                self.replace_chars(host, count, c);
            }
            Prefix::Find(find) => {
                self.last_find = Some((find, c));
                self.motion_key(host, Motion::Find(find, c), opts, clip);
            }
            Prefix::Object { around } => {
                let big = match c {
                    'w' => false,
                    'W' => true,
                    _ => {
                        self.pending = Pending::default();
                        return Outcome::Handled;
                    }
                };
                self.object(host, around, big, opts, clip);
            }
        }
        Outcome::Handled
    }

    /// A character key in Normal or Visual mode.
    fn char_command(
        &mut self,
        host: &mut H,
        c: char,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let visual = self.mode == Mode::Visual;
        // Counts: `0` is a motion unless a count has started.
        if let Some(digit) = c.to_digit(10)
            && (digit != 0 || self.count_slot().is_some())
        {
            let slot = self.count_slot_mut();
            *slot = Some(
                slot.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize)
                    .min(99_999),
            );
            return Outcome::Handled;
        }
        let motion = match c {
            'h' => Some(Motion::Left),
            'l' | ' ' => Some(Motion::Right),
            'j' => Some(Motion::Down),
            'k' => Some(Motion::Up),
            'w' => Some(Motion::Word { big: false }),
            'W' => Some(Motion::Word { big: true }),
            'b' => Some(Motion::Back { big: false }),
            'B' => Some(Motion::Back { big: true }),
            'e' => Some(Motion::End { big: false }),
            'E' => Some(Motion::End { big: true }),
            '0' => Some(Motion::LineStart),
            '^' => Some(Motion::FirstNonBlank),
            '$' => Some(Motion::LineEnd),
            'G' => Some(Motion::FileEnd),
            ';' => Some(Motion::RepeatFind { reverse: false }),
            ',' => Some(Motion::RepeatFind { reverse: true }),
            _ => None,
        };
        if let Some(motion) = motion {
            return self.motion_key(host, motion, opts, clip);
        }
        match c {
            'g' => self.pending.prefix = Prefix::G,
            'f' | 't' | 'F' | 'T' => {
                self.pending.prefix = Prefix::Find(Find {
                    forward: c == 'f' || c == 't',
                    till: c == 't' || c == 'T',
                });
            }
            'i' | 'a' if visual || self.pending.op.is_some() => {
                self.pending.prefix = Prefix::Object { around: c == 'a' };
            }
            'd' | 'c' | 'y' => {
                let op = match c {
                    'd' => Op::Delete,
                    'c' => Op::Change,
                    _ => Op::Yank,
                };
                if visual {
                    self.visual_op(host, op, opts, clip);
                } else if self.pending.op == Some(op) {
                    // `dd`, `cc`, `yy`: the line.
                    self.run(host, Motion::Lines, opts, clip);
                } else if self.pending.op.is_none() {
                    self.pending.op = Some(op);
                } else {
                    self.pending = Pending::default();
                }
            }
            _ if visual => self.visual_command(host, c, opts, clip),
            _ if self.pending.op.is_some() => {
                // Not a motion or an object: the operator is cancelled.
                self.pending = Pending::default();
            }
            _ => self.normal_command(host, c, opts, clip),
        }
        Outcome::Handled
    }

    fn count_slot(&self) -> Option<usize> {
        if self.pending.op.is_some() {
            self.pending.op_count
        } else {
            self.pending.count
        }
    }

    fn count_slot_mut(&mut self) -> &mut Option<usize> {
        if self.pending.op.is_some() {
            &mut self.pending.op_count
        } else {
            &mut self.pending.count
        }
    }

    fn motion_key(
        &mut self,
        host: &mut H,
        motion: Motion,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.run(host, motion, opts, clip);
        Outcome::Handled
    }

    /// Normal-mode commands that are not operators or motions.
    fn normal_command(&mut self, host: &mut H, c: char, opts: Options, clip: &mut dyn Clipboard) {
        let count = self.pending.total();
        let n = count.unwrap_or(1).max(1);
        match c {
            'x' => self.run_op(host, Op::Delete, Motion::Right, opts, clip),
            'X' => self.run_op(host, Op::Delete, Motion::Left, opts, clip),
            'D' => self.run_op(host, Op::Delete, Motion::LineEnd, opts, clip),
            'C' => self.run_op(host, Op::Change, Motion::LineEnd, opts, clip),
            's' => self.run_op(host, Op::Change, Motion::Right, opts, clip),
            'S' => self.run_op(host, Op::Change, Motion::Lines, opts, clip),
            'Y' => self.run_op(host, Op::Yank, Motion::Lines, opts, clip),
            'i' => {
                self.changed = true;
                self.pending = Pending::default();
                self.enter_insert(host);
            }
            'a' => {
                self.changed = true;
                self.pending = Pending::default();
                let at = host.cursor();
                let buf = Text::new(&*host);
                let to = (at < buf.line_end(at)).then(|| buf.next(at));
                if let Some(to) = to {
                    host.set_cursor(to);
                }
                self.enter_insert(host);
            }
            'I' => {
                self.changed = true;
                self.pending = Pending::default();
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start(host.cursor()));
                host.set_cursor(to);
                self.enter_insert(host);
            }
            'A' => {
                self.changed = true;
                self.pending = Pending::default();
                let to = Text::new(&*host).line_end(host.cursor());
                host.set_cursor(to);
                self.enter_insert(host);
            }
            'o' | 'O' => {
                self.changed = true;
                self.pending = Pending::default();
                let at = host.cursor();
                let buf = Text::new(&*host);
                if opts.single_line {
                    let to = if c == 'o' { buf.len() } else { 0 };
                    host.set_cursor(to);
                    self.enter_insert(host);
                } else {
                    let (le, ls) = (buf.line_end(at), buf.line_start(at));
                    host.begin_group();
                    host.apply(if c == 'o' {
                        Transaction::insert(le, "\n", le + 1)
                    } else {
                        Transaction::insert(ls, "\n", ls)
                    });
                    self.enter_insert(host);
                }
            }
            'p' | 'P' => {
                self.pending = Pending::default();
                self.paste(host, c == 'P', n, opts, clip);
            }
            'u' => {
                self.pending = Pending::default();
                for _ in 0..n {
                    if !host.undo() {
                        break;
                    }
                }
                self.want_col = None;
            }
            'r' => self.pending.prefix = Prefix::Replace,
            '~' => {
                self.pending = Pending::default();
                self.toggle_case(host, n);
            }
            'v' => {
                self.pending = Pending::default();
                self.anchor = host.cursor();
                self.mode = Mode::Visual;
                self.want_col = None;
            }
            _ => self.pending = Pending::default(),
        }
    }

    // ---- motions and operators -------------------------------------------------------------

    /// A motion key: moves the caret, or acts as the target of a pending operator.
    fn run(&mut self, host: &mut H, motion: Motion, opts: Options, clip: &mut dyn Clipboard) {
        if let Some(op) = self.pending.op {
            self.run_op(host, op, motion, opts, clip);
            return;
        }
        let count = self.pending.total();
        self.pending = Pending::default();
        let buf = Text::new(&*host);
        let from = host.cursor();
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.want_col = None;
        }
        let Some(target) = self.target(&buf, from, motion, count, false, opts) else {
            return;
        };
        if motion == Motion::LineEnd {
            self.want_col = Some(usize::MAX);
        }
        let to = buf.normal_clamp(target.to);
        host.set_cursor(to);
    }

    /// An operator and its motion (or the keys that stand for them: `x`, `D`, `cc`).
    fn run_op(
        &mut self,
        host: &mut H,
        op: Op,
        motion: Motion,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let count = self.pending.total();
        self.pending = Pending::default();
        let buf = Text::new(&*host);
        let from = host.cursor();
        if !matches!(motion, Motion::Up | Motion::Down) {
            self.want_col = None;
        }
        // `cw` on a word changes up to its end, like `ce` (but stays on a one-letter word).
        let on_word = buf.at(from).is_some_and(|c| class(c, false) != 0);
        let target = match (op, motion) {
            (Op::Change, Motion::Word { big }) if on_word => {
                let mut to = buf.run_end(from, big);
                for _ in 1..count.unwrap_or(1) {
                    to = buf.word_end(to, big);
                }
                Some(Target {
                    to,
                    kind: Kind::Inclusive,
                })
            }
            _ => self.target(&buf, from, motion, count, true, opts),
        };
        let Some(target) = target else {
            return;
        };
        let span = buf.span(from, target, opts.single_line);
        self.apply(host, op, span, from, opts, clip);
    }

    /// Operator on `iw`, `aw`, `iW`, `aW`.
    fn object(
        &mut self,
        host: &mut H,
        around: bool,
        big: bool,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let buf = Text::new(&*host);
        let from = host.cursor();
        let Some((start, end)) = buf.word_object(from, around, big) else {
            self.pending = Pending::default();
            return;
        };
        if self.mode == Mode::Visual {
            self.pending = Pending::default();
            if end > start {
                self.anchor = start;
                let to = buf.prev(end);
                host.set_cursor(to);
            }
            return;
        }
        let Some(op) = self.pending.op else {
            self.pending = Pending::default();
            return;
        };
        self.pending = Pending::default();
        let span = Span {
            start,
            end,
            linewise: false,
        };
        self.apply(host, op, span, from, opts, clip);
    }

    /// Applies `op` to `span`; `from` is where the caret was.
    fn apply(
        &mut self,
        host: &mut H,
        op: Op,
        span: Span,
        from: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let text = host.slice(span.start..span.end).into_owned();
        let empty = span.start == span.end && !span.linewise;
        if !empty {
            self.register = Register {
                text,
                linewise: span.linewise,
            };
            if !self.register.text.is_empty() {
                self.write_clipboard(opts, clip);
            }
        }
        match op {
            Op::Yank => {
                let at = if span.linewise {
                    let buf = Text::new(&*host);
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
            }
            Op::Change => {
                self.changed = true;
                host.begin_group();
                if !empty {
                    self.delete_span(host, span, true);
                }
                self.enter_insert(host);
            }
        }
    }

    /// Removes `span`. A deleted line takes its newline with it, unless `keep_line` (a changed
    /// line stays, empty).
    fn delete_span(&mut self, host: &mut H, span: Span, keep_line: bool) {
        if !span.linewise || keep_line {
            host.apply(Transaction::delete(span.start..span.end, span.start));
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
        host.apply(Transaction::delete(start..end, start));
        let after = Text::new(&*host);
        let line = line.min(after.line_count() - 1);
        let at = after.first_nonblank(after.line_start_of(line));
        host.set_cursor(at);
    }

    fn write_clipboard(&self, opts: Options, clip: &mut dyn Clipboard) {
        if opts.use_system_clipboard {
            clip.write(&self.register.clipboard_form());
        }
    }

    /// Where `motion` leads from `from`; `None` when it cannot move (the command does nothing).
    /// `op` is whether an operator waits for it (a few motions differ then).
    fn target(
        &mut self,
        buf: &Text<'_, H>,
        from: usize,
        motion: Motion,
        count: Option<usize>,
        op: bool,
        opts: Options,
    ) -> Option<Target> {
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
        match motion {
            Motion::Left => exclusive(buf.retreat(from, n, buf.line_start(from))),
            Motion::Right => {
                let limit = if op {
                    buf.line_end(from)
                } else {
                    buf.last_in_line(from).max(from)
                };
                exclusive(buf.advance(from, n, limit))
            }
            Motion::Up | Motion::Down => {
                if opts.single_line {
                    return None;
                }
                let current = buf.line_of(from);
                let total = buf.line_count();
                let line = if motion == Motion::Up {
                    if current == 0 {
                        return None;
                    }
                    current.saturating_sub(n)
                } else {
                    if current + 1 >= total {
                        return None;
                    }
                    (current + n).min(total - 1)
                };
                let col = *self.want_col.get_or_insert_with(|| buf.column(from));
                linewise(buf.at_column(buf.line_start_of(line), col))
            }
            Motion::Word { big } => {
                let mut to = from;
                let mut last_start = from;
                for _ in 0..n {
                    last_start = to;
                    to = buf.word_forward(to, big);
                }
                if op && to > from && buf.line_of(to) > buf.line_of(last_start) {
                    // The last word of a line: the operator stops at the line's end.
                    to = buf.line_end(last_start);
                }
                exclusive(to)
            }
            Motion::Back { big } => {
                let mut to = from;
                for _ in 0..n {
                    to = buf.word_back(to, big);
                }
                exclusive(to)
            }
            Motion::End { big } => {
                let mut to = from;
                for _ in 0..n {
                    to = buf.word_end(to, big);
                }
                inclusive(to)
            }
            Motion::LineStart => exclusive(buf.line_start(from)),
            Motion::FirstNonBlank => exclusive(buf.first_nonblank(buf.line_start(from))),
            Motion::LineEnd => {
                let line = (buf.line_of(from) + n - 1).min(buf.line_count() - 1);
                inclusive(buf.last_in_line(buf.line_start_of(line)))
            }
            Motion::FileStart | Motion::FileEnd => {
                let last = buf.line_count() - 1;
                let line = match (count, motion) {
                    (Some(n), _) => (n - 1).min(last),
                    (None, Motion::FileStart) => 0,
                    (None, _) => last,
                };
                let start = buf.line_start_of(line);
                linewise(buf.first_nonblank(start))
            }
            Motion::Find(find, ch) => buf.find(from, find, ch, n).map(|to| Target {
                to,
                kind: if find.forward {
                    Kind::Inclusive
                } else {
                    Kind::Exclusive
                },
            }),
            Motion::RepeatFind { reverse } => {
                let (find, ch) = self.last_find?;
                let find = Find {
                    forward: find.forward != reverse,
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
            Motion::Lines => {
                let line = (buf.line_of(from) + n - 1).min(buf.line_count() - 1);
                linewise(buf.line_start_of(line))
            }
        }
    }

    // ---- other commands --------------------------------------------------------------------

    fn replace_chars(&mut self, host: &mut H, count: usize, c: char) {
        let from = host.cursor();
        let buf = Text::new(&*host);
        let le = buf.line_end(from);
        // The `count` characters must all be on the line.
        let end = buf.advance(from, count, le);
        if c == '\n' || host.slice(from..end).chars().count() != count {
            return;
        }
        self.changed = true;
        let with: String = std::iter::repeat_n(c, count).collect();
        let cursor = from + with.len() - c.len_utf8();
        host.begin_group();
        host.apply(Transaction::replace(from..end, with, cursor));
    }

    fn toggle_case(&mut self, host: &mut H, count: usize) {
        let from = host.cursor();
        let buf = Text::new(&*host);
        let end = buf.advance(from, count, buf.line_end(from));
        if end == from {
            return;
        }
        self.changed = true;
        host.begin_group();
        let old = host.slice(from..end).into_owned();
        let chars = old.chars().count();
        host.apply(Transaction::replace(from..end, toggle_case_of(&old), from));
        let after = Text::new(&*host);
        let to = after.advance(from, chars, after.last_in_line(from).max(from));
        host.set_cursor(to);
    }

    fn paste(
        &mut self,
        host: &mut H,
        before: bool,
        count: usize,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let Some(mut register) = self.paste_source(opts, clip) else {
            return;
        };
        if opts.single_line {
            register.linewise = false;
            register.text = register.text.replace(['\r', '\n'], " ");
        }
        if register.text.is_empty() && !register.linewise {
            return;
        }
        self.changed = true;
        host.begin_group();
        let from = host.cursor();
        let buf = Text::new(&*host);
        if register.linewise {
            let block = vec![register.text.as_str(); count].join("\n");
            let tx = if before {
                let ls = buf.line_start(from);
                Transaction::insert(ls, format!("{block}\n"), ls)
            } else {
                let le = buf.line_end(from);
                Transaction::insert(le, format!("\n{block}"), le + 1)
            };
            host.apply(tx);
            let after = Text::new(&*host);
            let to = after.first_nonblank(host.cursor());
            host.set_cursor(to);
        } else {
            let block = register.text.repeat(count);
            let at = if before || buf.at(from).is_none_or(|c| c == '\n') {
                from
            } else {
                buf.next(from)
            };
            let cursor = if block.contains('\n') {
                at
            } else if let Some(last) = block.chars().next_back() {
                at + block.len() - last.len_utf8()
            } else {
                at
            };
            host.apply(Transaction::insert(at, block, cursor));
        }
    }

    /// The register, or the system clipboard when it holds something else.
    fn paste_source(&mut self, opts: Options, clip: &mut dyn Clipboard) -> Option<Register> {
        if opts.use_system_clipboard
            && let Some(text) = clip.read().filter(|t| !t.is_empty())
        {
            if self.register.clipboard_form() == text {
                return Some(self.register.clone());
            }
            return Some(match text.strip_suffix('\n') {
                Some(line) => Register {
                    text: line.to_owned(),
                    linewise: true,
                },
                None => Register {
                    text,
                    linewise: false,
                },
            });
        }
        (!self.register.text.is_empty() || self.register.linewise).then(|| self.register.clone())
    }

    fn repeat(&mut self, host: &mut H, opts: Options, clip: &mut dyn Clipboard) {
        let Some(change) = self.last_change.clone() else {
            return;
        };
        self.replaying = true;
        for key in change.keys {
            self.key(host, key, opts, clip);
        }
        if self.mode == Mode::Insert {
            if let Some(text) = &change.inserted {
                host.type_text(text);
            }
            self.leave_insert(host);
        }
        self.replaying = false;
        self.rec.clear();
        self.changed = false;
    }

    // ---- Visual ----------------------------------------------------------------------------

    fn leave_visual(&mut self, host: &mut H) {
        self.mode = Mode::Normal;
        self.pending = Pending::default();
        self.want_col = None;
        self.clamp_normal(host);
    }

    fn visual_span(&self, host: &H) -> Span {
        let buf = Text::new(host);
        let a = buf.boundary_up(self.anchor);
        let b = host.cursor();
        Span {
            start: a.min(b),
            end: buf.next(a.max(b)),
            linewise: false,
        }
    }

    fn visual_op(&mut self, host: &mut H, op: Op, opts: Options, clip: &mut dyn Clipboard) {
        let span = self.visual_span(host);
        let from = host.cursor();
        self.mode = Mode::Normal;
        self.pending = Pending::default();
        self.apply(host, op, span, from, opts, clip);
        // Changes made in Visual are not repeated (Vim repeats them on a same-sized region).
        self.changed = false;
        self.rec.clear();
    }

    fn visual_command(&mut self, host: &mut H, c: char, opts: Options, clip: &mut dyn Clipboard) {
        match c {
            'v' => self.leave_visual(host),
            'o' => {
                let cursor = host.cursor();
                host.set_cursor(self.anchor);
                self.anchor = cursor;
            }
            'x' => self.visual_op(host, Op::Delete, opts, clip),
            's' => self.visual_op(host, Op::Change, opts, clip),
            '~' => {
                let span = self.visual_span(host);
                self.mode = Mode::Normal;
                if span.end > span.start {
                    host.begin_group();
                    let toggled = toggle_case_of(&host.slice(span.start..span.end));
                    host.apply(Transaction::replace(
                        span.start..span.end,
                        toggled,
                        span.start,
                    ));
                }
                self.changed = false;
                self.rec.clear();
            }
            _ => {}
        }
        if self.mode == Mode::Visual {
            self.pending = Pending::default();
        }
    }

    /// Normal mode never rests past the last character of a line.
    fn clamp_normal(&mut self, host: &mut H) {
        if self.mode != Mode::Normal {
            return;
        }
        let at = host.cursor();
        let to = Text::new(&*host).normal_clamp(at);
        if to != at {
            host.set_cursor(to);
        }
    }
}

fn toggle_case_of(text: &str) -> String {
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
