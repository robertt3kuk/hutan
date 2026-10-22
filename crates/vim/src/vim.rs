//! The modal state machine: [`Vim`] turns [`Key`]s into motions, operators and edits on a
//! [`VimHost`]. The state lives in [`Core`], whose methods are generic over the host, so that a
//! `:g` or a macro can drive the same state over any host; `Vim<H>` ties it to one kind.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;

use crate::command::{self, Command, Context, Parse};
use crate::ex::Ex;
use crate::host::{Clipboard, Transaction, VimHost};
use crate::key::{Key, Mode, Options, Outcome};
use crate::marks::{Marks, Pos};
use crate::pattern::CaseOptions;
use crate::register::{Register, RegisterKind, Registers};
use crate::text::{Find, Text};

/// The options of the modal layer that `:set` changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub ignore_case: bool,
    pub smart_case: bool,
    pub wrap_scan: bool,
    pub hl_search: bool,
    pub inc_search: bool,
    pub expand_tab: bool,
    pub shift_round: bool,
    pub join_spaces: bool,
    pub shift_width: usize,
    pub tab_stop: usize,
}

impl Default for Settings {
    /// Vim's own defaults (`vim -u NONE`).
    fn default() -> Self {
        Self {
            ignore_case: false,
            smart_case: false,
            wrap_scan: true,
            hl_search: false,
            inc_search: false,
            expand_tab: false,
            shift_round: false,
            join_spaces: true,
            shift_width: 8,
            tab_stop: 8,
        }
    }
}

impl Settings {
    pub fn case(&self) -> CaseOptions {
        CaseOptions {
            ignore_case: self.ignore_case,
            smart_case: self.smart_case,
        }
    }

    /// The width a `>` shifts by (`'shiftwidth'`, or `'tabstop'` when it is 0).
    pub(crate) fn shift(&self) -> usize {
        if self.shift_width == 0 {
            self.tab_stop.max(1)
        } else {
            self.shift_width
        }
    }
}

/// An answer of the modal layer for the field to show (a `:` command's output, an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub error: bool,
}

/// The `:`, `/` or `?` line being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmdLine {
    pub kind: char,
    pub text: String,
}

/// The last change, for `.`: the keys of its command and the text typed in the Insert mode it
/// ended in.
#[derive(Debug, Clone, Default)]
pub(crate) struct Change {
    pub keys: Vec<Key>,
    pub inserted: Option<String>,
}

/// How an Insert session repeats its text when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// `3ix<Esc>`: the text again after itself.
    Inline,
    /// `3ox<Esc>`: on new lines.
    Lines,
}

/// The other lines of a Visual block an Insert session's text goes to when it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockInsert {
    pub lines: Vec<usize>,
    /// The display column the text goes in at.
    pub col: usize,
    /// `$A`: at each line's end.
    pub at_end: bool,
    /// `A`: lines that end before the column are padded with spaces up to it.
    pub pad: bool,
    /// Where the caret rests afterwards (`I` and `A`: the block's corner; `c`: on the last
    /// character typed, like any Insert).
    pub corner: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct InsertSession {
    pub origin: usize,
    pub tail: usize,
    pub count: usize,
    pub repeat: Repeat,
    pub block: Option<BlockInsert>,
    /// The keys typed in the session as Vim stores them in `".` (`None` once the host typed
    /// for itself).
    pub typed: Option<String>,
}

/// The last Visual selection, for `gv`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VisualMemory {
    pub mode: Mode,
    pub anchor: Pos,
    pub cursor: Pos,
    pub want_col: Option<usize>,
}

/// The last search: what `n`, `N` and `:s//` use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastSearch {
    pub pattern: String,
    pub forward: bool,
    pub offset: String,
}

/// The last `:s`: what `:s`, `:&` and `&` repeat.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LastSubstitute {
    pub pattern: String,
    pub replacement: String,
    pub global: bool,
    pub ignore_case: Option<bool>,
}

/// How lines moved in one edit (old line numbers), for marks and `:g`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineChange {
    /// Lines `first..first + removed` became `inserted` whole lines.
    Lines {
        first: usize,
        removed: usize,
        inserted: usize,
    },
    /// Lines `first..=last` became `inserted + 1` lines (a change inside lines).
    Merge {
        first: usize,
        last: usize,
        inserted: usize,
    },
}

/// The modal state of one field; the text stays in the host.
#[derive(Debug, Clone, Default)]
pub(crate) struct Core {
    pub mode: Mode,
    /// The keys of the Normal or Visual command in progress.
    pub pending: Vec<Key>,
    pub cmdline: Option<CmdLine>,
    /// Where Visual started (byte offset).
    pub anchor: usize,
    /// The column `j`/`k` aim for, in characters (`usize::MAX` after `$`).
    pub want_col: Option<usize>,
    pub regs: Registers,
    pub marks: Marks,
    pub settings: Settings,
    pub last_find: Option<(Find, char)>,
    pub last_search: Option<LastSearch>,
    pub last_sub: Option<LastSubstitute>,
    /// What `~` in a pattern or replacement stands for.
    pub last_replacement: Option<String>,
    /// Keys of the Normal-mode command in progress, for `.`.
    pub rec: Vec<Key>,
    /// The command in progress changed the text.
    pub changed: bool,
    pub last_change: Option<Change>,
    pub replaying: bool,
    pub insert: Option<InsertSession>,
    /// `q{reg}`: the register and the keys typed since.
    pub recording: Option<(char, Vec<Key>)>,
    pub last_macro: Option<char>,
    /// How deep macros, `.`, `:normal` and `:g` are nested: the undo group stays open inside.
    pub depth: usize,
    /// A command failed (Vim beeps): the rest of a macro or `:normal` is dropped.
    pub failed: bool,
    pub last_visual: Option<VisualMemory>,
    pub message: Option<Message>,
    /// The lines `:g` and `:normal` still have to visit (innermost last), moved with every
    /// edit; a removed line becomes `None`.
    pub line_lists: Vec<Vec<Option<usize>>>,
    /// `⌃R` in Insert mode waits for the register's name.
    pub insert_register: bool,
    /// `⌃O` in Insert mode: the Normal command after which Insert comes back.
    pub insert_once: Option<crate::insert::InsertOnce>,
    /// How many lines `⌃D` / `⌃U` scroll once a count set it (Vim's `'scroll'`); half the
    /// window until then.
    pub scroll_lines: Option<usize>,
}

/// The modal state of one field. The text stays in the [`VimHost`] the keys are applied to.
///
/// `H` ties the state to the kind of host it works on; a `Vim` holds no host, every call takes
/// one.
pub struct Vim<H: VimHost> {
    pub(crate) core: Core,
    host: PhantomData<fn(&mut H)>,
}

impl<H: VimHost> Default for Vim<H> {
    fn default() -> Self {
        Self {
            core: Core::default(),
            host: PhantomData,
        }
    }
}

impl<H: VimHost> Clone for Vim<H> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            host: PhantomData,
        }
    }
}

impl<H: VimHost> fmt::Debug for Vim<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.core.fmt(f)
    }
}

/// The text a key stands for in a register (a macro): Vim's bytes for Escape, Return, ⌃ keys.
pub(crate) fn key_text(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_string(),
        Key::Ctrl(c) => {
            let code = (c.to_ascii_uppercase() as u32).wrapping_sub(64);
            char::from_u32(code).map_or_else(String::new, |c| c.to_string())
        }
        Key::Escape => "\u{1b}".into(),
        Key::Enter => "\r".into(),
        Key::Tab => "\t".into(),
        Key::Backspace => "\u{80}kb".into(),
        Key::Delete => "\u{80}kD".into(),
        Key::Left => "\u{80}kl".into(),
        Key::Right => "\u{80}kr".into(),
        Key::Up => "\u{80}ku".into(),
        Key::Down => "\u{80}kd".into(),
        Key::Home => "\u{80}kh".into(),
        Key::End => "\u{80}@7".into(),
    }
}

/// The keys a register's text stands for (a macro played with `@`).
pub(crate) fn text_keys(text: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(match c {
            '\u{1b}' => Key::Escape,
            '\r' | '\n' => Key::Enter,
            '\t' => Key::Tab,
            '\u{8}' | '\u{7f}' => Key::Backspace,
            '\u{80}' => {
                let code: String = chars.by_ref().take(2).collect();
                match code.as_str() {
                    "kb" => Key::Backspace,
                    "kD" => Key::Delete,
                    "kl" => Key::Left,
                    "kr" => Key::Right,
                    "ku" => Key::Up,
                    "kd" => Key::Down,
                    "kh" => Key::Home,
                    "@7" => Key::End,
                    _ => continue,
                }
            }
            // ⌃A…⌃Z are letters, ⌃\ ⌃] ⌃^ ⌃_ the characters after `[`.
            c if (c as u32) < 32 => {
                let base = if (c as u32) < 27 { 96 } else { 64 };
                match char::from_u32(c as u32 + base) {
                    Some(letter) => Key::Ctrl(letter),
                    None => continue,
                }
            }
            c => Key::Char(c),
        });
    }
    out
}

impl<H: VimHost> Vim<H> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.core.mode
    }

    /// Back to Insert with nothing pending (a field that opens for typing). Registers, marks
    /// and the last change survive.
    pub fn reset(&mut self) {
        let c = &mut self.core;
        c.mode = Mode::Insert;
        c.pending.clear();
        c.cmdline = None;
        c.rec.clear();
        c.changed = false;
        c.want_col = None;
        c.insert = None;
        c.depth = 0;
        c.failed = false;
        c.insert_register = false;
        c.insert_once = None;
    }

    /// The keys typed so far of a command in progress (`2d`), or the `:` / `/` / `?` line
    /// being typed (`/foo`), for display.
    pub fn pending_keys(&self) -> String {
        if let Some(line) = &self.core.cmdline {
            return format!("{}{}", line.kind, line.text);
        }
        self.core
            .pending
            .iter()
            .map(|k| match k {
                Key::Char(c) => c.to_string(),
                Key::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
                _ => String::new(),
            })
            .collect()
    }

    /// The `:` / `/` / `?` line being typed, with its leading character.
    pub fn command_line(&self) -> Option<String> {
        self.core
            .cmdline
            .as_ref()
            .map(|l| format!("{}{}", l.kind, l.text))
    }

    /// Whether `c`, typed now, is text rather than a command key: Insert mode, the `:` / `/` /
    /// `?` line, or a command that waits for its character (`f`, `t`, `r`). A client on a
    /// keyboard layout that does not type Latin letters reads every other key by its place on
    /// the keyboard instead (`d` where the key types `в`).
    pub fn takes_text(&self, c: char) -> bool {
        let core = &self.core;
        if core.mode == Mode::Insert || core.cmdline.is_some() {
            return true;
        }
        let mut keys = core.pending.clone();
        keys.push(Key::Char(c));
        let cx = Context {
            visual: core.mode.is_visual(),
            recording: core.recording.is_some(),
            search: None,
        };
        matches!(command::parse(&keys, cx), Parse::Done(_))
    }

    /// Whether `key` is the modal layer's in Insert mode in a field that types for itself
    /// ([`Options::insert_passthrough`]): Escape, Vim's Insert ⌃ keys, and the register name
    /// after `⌃R`. `false` outside Insert mode.
    pub fn takes_insert_key(&self, key: Key) -> bool {
        self.core.mode == Mode::Insert && self.core.takes_insert_key(key)
    }

    /// The register being recorded into (`q{reg}`).
    pub fn recording(&self) -> Option<char> {
        self.core.recording.as_ref().map(|(r, _)| *r)
    }

    /// What the unnamed register holds, and whether it is whole lines.
    pub fn register(&self) -> (&str, bool) {
        // The unnamed register is one of the stored ones; borrow its text through the store.
        match self.core.regs.unnamed_ref() {
            Some(reg) => (reg.text.as_str(), reg.kind == RegisterKind::Line),
            None => ("", false),
        }
    }

    /// The register `name` (`'"'` the unnamed one, `'.'` `':'` `'/'` the read-only ones).
    pub fn register_of(&self, name: char) -> Option<Register> {
        self.core.regs.get(name)
    }

    /// The options `:set` changes.
    pub fn settings(&self) -> Settings {
        self.core.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.core.settings
    }

    /// The answer of the last command (a `:` command's output, "Not supported: …", an error).
    pub fn message(&self) -> Option<&Message> {
        self.core.message.as_ref()
    }

    /// Takes the answer of the last command.
    pub fn take_message(&mut self) -> Option<Message> {
        self.core.message.take()
    }

    /// The 0-based line of mark `name` (`'a`, `'<`, …).
    pub fn mark_line(&self, name: char) -> Option<usize> {
        self.core.marks.get(name).map(|p| p.line)
    }

    /// A jump the host made itself (go to definition in the same file): where the caret is
    /// now becomes the newest jumplist entry and `''`, so `⌃O` comes back to it.
    pub fn push_jump(&mut self, host: &H) {
        let at = Core::pos(host, host.cursor());
        self.core.marks.jump_from(at);
    }

    /// Jumplist entries pushed so far: it grows with every jump (a host that moves the caret
    /// itself can tell whether Vim jumped since).
    pub fn jump_count(&self) -> u64 {
        self.core.marks.pushed()
    }

    /// The selected bytes in a Visual mode, from the first selected character to the end of the
    /// last (a block's whole rectangle as one range: see [`selections`](Self::selections)).
    pub fn selection(&self, host: &H) -> Option<Range<usize>> {
        let ranges = self.core.visual_ranges(host);
        let first = ranges.first()?.start;
        let last = ranges.last()?.end;
        Some(first..last.max(first))
    }

    /// The selected bytes in a Visual mode, one range per line for a block.
    pub fn selections(&self, host: &H) -> Vec<Range<usize>> {
        self.core.visual_ranges(host)
    }

    /// Applies a key to `host`.
    pub fn key(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.core.key(host, key, opts, clip)
    }

    /// Runs one `:` line (without the colon) as if typed after `:` in Normal mode.
    pub fn ex(&mut self, host: &mut H, line: &str, opts: Options, clip: &mut dyn Clipboard) {
        self.core.ex_line(host, line, opts, clip);
    }

    /// Runs a parsed `:` command. A command for the host's owner (files, windows) answers "Not
    /// supported".
    pub fn run(&mut self, host: &mut H, command: Ex, opts: Options, clip: &mut dyn Clipboard) {
        self.core.message = None;
        let result = self.core.run_ex(host, command, opts, clip);
        if let Err(e) = result {
            self.core.error(e.0);
        }
        self.core.close_group(host);
    }

    /// The 0-based lines of `spec` (`default` without one): marks, searches and the caret's
    /// line resolved against `host`.
    pub fn line_range(
        &mut self,
        host: &H,
        spec: Option<&crate::ex::LineSpec>,
        default: &crate::ex::LineSpec,
    ) -> Result<Range<usize>, crate::ex::ExError> {
        self.core.lines(host, spec, default)
    }

    /// Readies a `:s` for a host that runs it itself (in the background): the last search
    /// pattern for an empty one, the last replacement for `:s` / `:&` (and `~` in a new one),
    /// and remembers both. Returns the compiled pattern.
    pub fn prepare_substitute(
        &mut self,
        command: &mut crate::ex::Substitute,
    ) -> Result<crate::pattern::Pattern, crate::ex::ExError> {
        self.core.prepare_substitute(command)
    }

    /// The pattern of a `:g` a host runs itself: the last search pattern for an empty one;
    /// remembered as the last search (`n`, `"/`). Compiled with `'ignorecase'` /
    /// `'smartcase'`.
    pub fn search_pattern(
        &mut self,
        pattern: &str,
    ) -> Result<crate::pattern::Pattern, crate::ex::ExError> {
        self.core.global_pattern(pattern)
    }

    /// A `:` line the host's owner read and ran (a file tab draws its own `:` line): it
    /// becomes `":` (for `@:`), and a macro being recorded gets it as typed.
    pub fn command_line_ran(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        self.core.regs.last_ex = Some(line.to_owned());
        if let Some((_, keys)) = &mut self.core.recording {
            keys.push(Key::Char(':'));
            keys.extend(line.chars().map(Key::Char));
            keys.push(Key::Enter);
        }
    }

    /// `:normal keys` on each of `lines` (0-based, in order), in one undo step: the caret goes
    /// to the start of the line, the keys run as typed in Normal mode, and whatever they leave
    /// unfinished ends as with Escape. Lines removed by an earlier line's keys are skipped.
    pub fn normal_on_lines(
        &mut self,
        host: &mut H,
        lines: &[usize],
        keys: &str,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        self.core
            .normal_on_lines(host, Some(lines), keys, opts, clip);
        self.core.close_group(host);
    }
}

impl Core {
    // ---- messages ----------------------------------------------------------------------------

    pub fn say(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
        });
    }

    /// The command failed: Vim beeps, and a macro or `:normal` stops here.
    pub fn fail(&mut self) {
        self.failed = true;
    }

    /// The host's text with this state's `'tabstop'` (for display columns).
    pub fn text<'h, H: VimHost + ?Sized>(&self, host: &'h H) -> Text<'h, H> {
        Text::new(host).tabs(self.settings.tab_stop)
    }

    pub fn pos<H: VimHost + ?Sized>(host: &H, at: usize) -> Pos {
        let text = Text::new(host);
        let at = at.min(text.len());
        let ls = text.line_start(at);
        Pos {
            line: text.line_of(at),
            col: at - ls,
        }
    }

    /// The offset of `pos` in `host` (clamped to its line).
    pub fn offset<H: VimHost + ?Sized>(host: &H, pos: Pos) -> usize {
        let text = Text::new(host);
        let ls = text.line_start_of(pos.line);
        let le = text.line_end(ls);
        text.boundary_up((ls + pos.col).min(le)).min(le)
    }

    // ---- keys --------------------------------------------------------------------------------

    pub fn key<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        self.failed = false;
        if let Some((_, keys)) = &mut self.recording {
            keys.push(key);
        }
        self.process(host, key, opts, clip)
    }

    /// Keys that are not typed (a macro, `.`, `:normal`): run until one fails, in the undo step
    /// of the command that feeds them. Insert mode types them itself.
    pub fn feed<H: VimHost>(
        &mut self,
        host: &mut H,
        keys: &[Key],
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        if self.depth > 200 {
            self.error("E169: Command too recursive");
            self.fail();
            return;
        }
        let opts = Options {
            insert_passthrough: false,
            ..opts
        };
        self.depth += 1;
        for &key in keys {
            self.process(host, key, opts, clip);
            if self.failed {
                break;
            }
        }
        self.depth -= 1;
    }

    fn process<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        if self.cmdline.is_some() {
            return self.cmdline_key(host, key, opts, clip);
        }
        match self.mode {
            Mode::Insert => self.insert_key(host, key, opts, clip),
            _ => self.command_key(host, key, opts, clip),
        }
    }

    /// Ends an undo group, unless a macro, `.` or `:normal` is still running.
    pub fn close_group<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        if self.depth == 0 {
            host.end_group();
        }
    }

    pub fn idle(&self) -> bool {
        self.pending.is_empty() && self.cmdline.is_none()
    }

    // ---- edits -------------------------------------------------------------------------------

    /// How `edit` (in the current text) moves lines.
    fn line_change<H: VimHost + ?Sized>(
        host: &H,
        range: &Range<usize>,
        text: &str,
    ) -> Option<LineChange> {
        let buf = Text::new(host);
        let inserted = text.matches('\n').count();
        let (l1, l2) = (buf.line_of(range.start), buf.line_of(range.end));
        let removed = l2 - l1;
        if inserted == 0 && removed == 0 {
            return None;
        }
        let at_start = |i: usize| buf.line_start(i) == i;
        let whole = at_start(range.start)
            && at_start(range.end)
            && range.end > range.start
            && (text.is_empty() || text.ends_with('\n'));
        let pure_lines = at_start(range.start) && range.start == range.end && text.ends_with('\n');
        if whole || pure_lines {
            return Some(LineChange::Lines {
                first: l1,
                removed,
                inserted,
            });
        }
        // A last line removed with the newline before it.
        if buf.at(range.start) == Some('\n') && range.end == buf.len() && text.is_empty() {
            return Some(LineChange::Lines {
                first: l1 + 1,
                removed,
                inserted: 0,
            });
        }
        Some(LineChange::Merge {
            first: l1,
            last: l2,
            inserted,
        })
    }

    fn moved(&mut self, changes: &[LineChange]) {
        for change in changes.iter().rev() {
            self.marks.apply(*change);
            for lines in &mut self.line_lists {
                for line in lines.iter_mut() {
                    let Some(l) = *line else { continue };
                    *line = match *change {
                        LineChange::Lines {
                            first,
                            removed,
                            inserted,
                        } => {
                            if l >= first + removed {
                                Some(l + inserted - removed)
                            } else if l >= first {
                                None
                            } else {
                                Some(l)
                            }
                        }
                        LineChange::Merge {
                            first,
                            last,
                            inserted,
                        } => {
                            if l > last {
                                Some(l + inserted - (last - first))
                            } else if l > first {
                                None
                            } else {
                                Some(l)
                            }
                        }
                    };
                }
            }
        }
    }

    /// Every edit the layer makes goes through here: marks and `:g` follow the lines.
    pub fn apply<H: VimHost + ?Sized>(&mut self, host: &mut H, tx: Transaction) {
        let changes: Vec<LineChange> = tx
            .edits
            .iter()
            .filter_map(|e| Self::line_change(host, &e.range, &e.text))
            .collect();
        host.apply(tx);
        self.moved(&changes);
    }

    pub fn type_text<H: VimHost + ?Sized>(&mut self, host: &mut H, text: &str) {
        let at = host.cursor();
        let change = Self::line_change(host, &(at..at), text);
        host.type_text(text);
        self.moved(&change.into_iter().collect::<Vec<_>>());
    }

    fn host_backspace<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        let at = host.cursor();
        let change = (at > 0 && host.char_before(at) == Some('\n'))
            .then(|| Self::line_change(host, &(at - 1..at), ""))
            .flatten();
        host.backspace();
        self.moved(&change.into_iter().collect::<Vec<_>>());
    }

    fn host_delete<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        let at = host.cursor();
        let change = (host.char_at(at) == Some('\n'))
            .then(|| Self::line_change(host, &(at..at + 1), ""))
            .flatten();
        host.delete_forward();
        self.moved(&change.into_iter().collect::<Vec<_>>());
    }

    // ---- Insert ------------------------------------------------------------------------------

    fn insert_key<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        // `i_CTRL-C` ends Insert as Escape does (a real Vim repeats a count's text for a typed
        // `⌃C` too, unless the key interrupted it).
        if !self.insert_register && matches!(key, Key::Escape | Key::Ctrl('c')) {
            self.leave_insert(host, opts);
            return Outcome::Handled;
        }
        if let Some(record) = self.insert_control(host, key, opts, clip) {
            if let Some(session) = self.insert.as_mut() {
                // A field that types for itself typed keys the layer never saw: `".` is then
                // the text the session left.
                match (record, &mut session.typed) {
                    (_, typed) if opts.insert_passthrough => *typed = None,
                    (Some(text), Some(typed)) => typed.push_str(&text),
                    _ => {}
                }
            }
            return Outcome::Handled;
        }
        if opts.insert_passthrough {
            if let Some(session) = &mut self.insert {
                session.typed = None;
            }
            return Outcome::Ignored;
        }
        match key {
            Key::Char(c) => self.type_text(host, c.encode_utf8(&mut [0; 4])),
            Key::Enter if !opts.single_line => self.type_text(host, "\n"),
            Key::Backspace => self.host_backspace(host),
            Key::Delete => self.host_delete(host),
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
        if let Some(typed) = self.insert.as_mut().and_then(|s| s.typed.as_mut()) {
            // `".` holds a typed line break as a newline (a macro holds Return).
            if key == Key::Enter {
                typed.push('\n');
            } else {
                typed.push_str(&key_text(key));
            }
        }
        Outcome::Handled
    }

    pub fn enter_insert<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        count: usize,
        repeat: Repeat,
        block: Option<BlockInsert>,
    ) {
        host.begin_group();
        self.changed = true;
        self.mode = Mode::Insert;
        // A command after `⌃O` that starts Insert itself (`a`, `o`): no second return.
        self.insert_once = None;
        let cursor = host.cursor();
        self.insert = Some(InsertSession {
            origin: cursor,
            tail: host.len() - cursor,
            count: count.max(1),
            repeat,
            block,
            typed: Some(String::new()),
        });
        self.pending.clear();
    }

    pub fn leave_insert<H: VimHost>(&mut self, host: &mut H, opts: Options) {
        self.finish_insert(host, opts, true);
    }

    /// Ends Insert mode; the caret steps back onto the last character typed unless `step_back`
    /// is off (`⌃O`).
    pub(crate) fn finish_insert<H: VimHost>(
        &mut self,
        host: &mut H,
        opts: Options,
        step_back: bool,
    ) {
        let session = self.insert.take();
        let inserted = session.as_ref().and_then(|s| {
            let cursor = host.cursor();
            (s.origin <= cursor
                && host.len() - cursor == s.tail
                && host.char_at(s.origin).is_some()
                && s.origin < cursor)
                .then(|| host.slice(s.origin..cursor).into_owned())
        });
        if let (Some(s), Some(text)) = (&session, &inserted) {
            if s.count > 1 {
                let piece = match s.repeat {
                    Repeat::Inline => text.clone(),
                    Repeat::Lines => format!("\n{text}"),
                };
                let more = piece.repeat(s.count - 1);
                let at = host.cursor();
                self.apply(host, Transaction::insert(at, more.clone(), at + more.len()));
            }
            if let Some(block) = &s.block
                && !text.contains('\n')
            {
                self.block_insert_rest(host, block, text);
            }
        }
        let typed = session
            .as_ref()
            .and_then(|s| s.typed.clone())
            .filter(|t| !t.is_empty());
        if let Some(text) = typed.or_else(|| inserted.clone()) {
            self.regs.last_insert = Some(text);
        }
        if let Some(text) = &inserted
            && opts.insert_passthrough
            && self.depth == 0
            && let Some((_, keys)) = &mut self.recording
        {
            // The host typed the text itself: the macro gets it before the Escape.
            let escape = keys.pop();
            keys.extend(text.chars().map(Key::Char));
            keys.extend(escape);
        }
        if !self.rec.is_empty() && !self.replaying {
            self.last_change = Some(Change {
                keys: std::mem::take(&mut self.rec),
                inserted,
            });
        }
        self.rec.clear();
        self.changed = false;
        let at = host.cursor();
        self.marks.last_insert = Some(Self::pos(host, at));
        self.marks.last_change = Some(Self::pos(host, at));
        self.close_group(host);
        self.mode = Mode::Normal;
        self.want_col = None;
        // Like Vim, the caret steps back onto the last character typed (a block `I` / `A`
        // rests on the block's corner).
        if !step_back {
            return;
        }
        let buf = Text::new(&*host);
        let at = host.cursor();
        if let Some(corner) = session
            .as_ref()
            .and_then(|s| s.block.as_ref())
            .and_then(|b| b.corner)
        {
            host.set_cursor(Text::new(&*host).normal_clamp(corner));
            return;
        }
        // A caret after the final newline (the host's empty last line) goes back onto the last
        // line, which Vim's buffer ends with.
        let to = if at > buf.line_start(at) {
            buf.prev(at)
        } else {
            at
        };
        let to = buf.normal_clamp(to);
        if to != at {
            host.set_cursor(to);
        }
    }

    /// The text typed on a block's first line goes to its other lines.
    fn block_insert_rest<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        block: &BlockInsert,
        text: &str,
    ) {
        let buf = self.text(&*host);
        let mut edits = Vec::new();
        for &line in &block.lines {
            if line >= buf.line_count() {
                break;
            }
            let ls = buf.line_start_of(line);
            let le = buf.line_end(ls);
            let width = buf.column(le);
            if block.at_end {
                edits.push(crate::host::Edit {
                    range: le..le,
                    text: text.to_owned(),
                });
            } else if width >= block.col {
                let at = buf.at_vcol(ls, block.col);
                edits.push(crate::host::Edit {
                    range: at..at,
                    text: text.to_owned(),
                });
            } else if block.pad {
                let pad = " ".repeat(block.col - width);
                edits.push(crate::host::Edit {
                    range: le..le,
                    text: format!("{pad}{text}"),
                });
            }
        }
        if edits.is_empty() {
            return;
        }
        let cursor = host.cursor();
        self.apply(host, Transaction { edits, cursor });
    }

    // ---- the `:` / `/` / `?` line --------------------------------------------------------------

    fn cmdline_key<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let Some(line) = self.cmdline.as_mut() else {
            return Outcome::Ignored;
        };
        let search = line.kind != ':';
        if search && !self.replaying && !self.mode.is_visual() {
            self.rec.push(key);
        }
        match key {
            Key::Char(c) => line.text.push(c),
            Key::Tab => line.text.push('\t'),
            Key::Backspace => {
                if line.text.pop().is_none() {
                    self.cancel_line(host);
                }
            }
            Key::Ctrl('u') => line.text.clear(),
            Key::Escape | Key::Ctrl('c') => self.cancel_line(host),
            Key::Enter => {
                let Some(line) = self.cmdline.take() else {
                    return Outcome::Handled;
                };
                if line.kind == ':' {
                    self.pending.clear();
                    self.rec.clear();
                    let text = line.text.clone();
                    self.ex_line(host, &text, opts, clip);
                } else {
                    let keys = std::mem::take(&mut self.pending);
                    let cx = Context {
                        visual: self.mode.is_visual(),
                        recording: self.recording.is_some(),
                        search: Some(&line.text),
                    };
                    match command::parse(&keys, cx) {
                        Parse::Done(command) => self.execute(host, command, opts, clip),
                        _ => self.fail(),
                    }
                }
                self.after_command(host);
            }
            _ => {}
        }
        Outcome::Handled
    }

    fn cancel_line<H: VimHost>(&mut self, host: &mut H) {
        self.cmdline = None;
        self.pending.clear();
        self.rec.clear();
        self.after_command(host);
    }

    /// Runs a `:` line: remembered in `":`, parsed, run; errors become the message.
    pub fn ex_line<H: VimHost>(
        &mut self,
        host: &mut H,
        line: &str,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        self.message = None;
        if self.mode.is_visual() {
            self.leave_visual(host);
        }
        if !line.trim().is_empty() {
            self.regs.last_ex = Some(line.to_owned());
        }
        let result = match crate::ex::parse(line) {
            Ok(Some(command)) => self.run_ex(host, command, opts, clip),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            self.error(e.0);
            self.fail();
        }
        self.close_group(host);
        self.clamp_normal(host);
    }

    // ---- Normal and Visual -------------------------------------------------------------------

    fn command_key<H: VimHost>(
        &mut self,
        host: &mut H,
        key: Key,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Outcome {
        let visual = self.mode.is_visual();
        // `⌃C` cancels like Escape: a command in progress, a Visual mode.
        let key = if key == Key::Ctrl('c') {
            Key::Escape
        } else {
            key
        };
        if self.pending.is_empty() {
            // Keys that are the field's when no command is in progress.
            let ignored = match key {
                Key::Escape => !visual,
                Key::Enter => true,
                Key::Tab => self.depth == 0,
                // The rest (`⌃W` `⌃]` `⌃T` `⌃^` `⌃G`, …) are the owner's: windows, files.
                Key::Ctrl(c) => !"rvoidufbeyaxl".contains(c),
                _ => false,
            };
            if ignored {
                // After `⌃O` any key is the command, Insert comes back.
                let once = self.insert_once.is_some();
                self.rec.clear();
                self.after_command(host);
                return if once {
                    Outcome::Handled
                } else {
                    Outcome::Ignored
                };
            }
            self.message = None;
        }
        if !visual && !self.replaying {
            self.rec.push(key);
        }
        self.pending.push(key);
        let cx = Context {
            visual,
            recording: self.recording.is_some(),
            search: None,
        };
        match command::parse(&self.pending, cx) {
            Parse::Incomplete => {}
            Parse::Done(command) => {
                self.pending.clear();
                self.execute(host, command, opts, clip);
            }
            Parse::Cancel => {
                self.pending.clear();
                self.rec.clear();
            }
            Parse::Invalid => {
                self.pending.clear();
                self.rec.clear();
                self.fail();
            }
            Parse::Unsupported(what) => {
                self.pending.clear();
                self.rec.clear();
                self.error(format!("Not supported: {what}"));
                self.fail();
            }
            Parse::Line(kind) if !opts.command_line && self.depth == 0 => {
                self.pending.clear();
                self.rec.clear();
                self.error(format!("Not supported here: {kind}"));
                self.fail();
            }
            Parse::Line(kind) => {
                let text = if kind == ':' {
                    // `3:` is `:.,.+2`; Visual starts with its lines.
                    if visual {
                        "'<,'>".to_owned()
                    } else {
                        match self.count_of_pending() {
                            Some(n) if n > 1 => format!(".,.+{}", n - 1),
                            Some(_) => ".".to_owned(),
                            None => String::new(),
                        }
                    }
                } else {
                    String::new()
                };
                if kind == ':' && visual {
                    self.leave_visual(host);
                }
                self.cmdline = Some(CmdLine { kind, text });
            }
        }
        self.after_command(host);
        Outcome::Handled
    }

    /// The count typed before a `:`.
    fn count_of_pending(&self) -> Option<usize> {
        let mut n: Option<usize> = None;
        for key in &self.pending {
            match key {
                Key::Char(d) if d.is_ascii_digit() => {
                    n = Some(n.unwrap_or(0) * 10 + d.to_digit(10).unwrap_or(0) as usize);
                }
                _ => break,
            }
        }
        n
    }

    /// After every key of a command: a finished Normal command is the last change (when it
    /// changed something), closes its undo step and leaves the caret on a character.
    pub fn after_command<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        if !self.idle() {
            return;
        }
        if self.mode == Mode::Normal {
            if self.changed && !self.replaying {
                self.last_change = Some(Change {
                    keys: std::mem::take(&mut self.rec),
                    inserted: None,
                });
            }
            self.rec.clear();
            self.changed = false;
            self.close_group(host);
            self.clamp_normal(host);
            self.insert_once_end(host);
        } else if self.mode.is_visual() {
            self.rec.clear();
            self.changed = false;
        }
    }

    pub fn execute<H: VimHost>(
        &mut self,
        host: &mut H,
        command: Command,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        use command::Action;
        match command.action {
            Action::Move(motion) => self.do_move(host, &motion, command.count, opts),
            Action::Operate(op, target) => {
                self.operate(host, op, &target, command.reg, command.count, opts, clip)
            }
            Action::VisualOperate(op, widen) => {
                self.visual_operate(host, op, widen, command.reg, command.count, opts, clip)
            }
            Action::Object(object, around) => {
                self.visual_object(host, object, around, command.count)
            }
            Action::Simple(simple) => {
                self.simple(host, simple, command.reg, command.count, opts, clip)
            }
        }
    }

    /// Normal mode never rests past the last character of a line.
    pub fn clamp_normal<H: VimHost + ?Sized>(&mut self, host: &mut H) {
        if self.mode != Mode::Normal {
            return;
        }
        let at = host.cursor();
        let to = Text::new(&*host).normal_clamp(at);
        if to != at {
            host.set_cursor(to);
        }
    }

    // ---- `.` and macros ------------------------------------------------------------------------

    pub fn repeat<H: VimHost>(
        &mut self,
        host: &mut H,
        count: Option<usize>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let Some(change) = self.last_change.clone() else {
            self.fail();
            return;
        };
        let mut keys = change.keys.clone();
        if let Some(count) = count {
            // The new count replaces the one the change was made with.
            let mut i = 0;
            let mut prefix = Vec::new();
            loop {
                match keys.get(i) {
                    Some(Key::Char('"')) if keys.len() > i + 1 => {
                        prefix.extend_from_slice(&keys[i..i + 2]);
                        i += 2;
                    }
                    Some(Key::Char(d)) if d.is_ascii_digit() && (*d != '0' || i > prefix.len()) => {
                        i += 1
                    }
                    _ => break,
                }
            }
            let mut out = prefix;
            out.extend(count.to_string().chars().map(Key::Char));
            out.extend_from_slice(&keys[i..]);
            keys = out;
        }
        self.replaying = true;
        self.rec.clear();
        self.depth += 1;
        let inner = Options {
            insert_passthrough: false,
            ..opts
        };
        for key in keys {
            self.process(host, key, inner, clip);
            if self.failed {
                break;
            }
        }
        if self.mode == Mode::Insert {
            if let Some(text) = &change.inserted {
                self.type_text(host, text);
            }
            self.leave_insert(host, inner);
        }
        self.depth -= 1;
        self.replaying = false;
        self.rec.clear();
        self.changed = false;
        let mut stored = change;
        if count.is_some() {
            stored.keys = self
                .last_change
                .as_ref()
                .map_or(stored.keys, |c| c.keys.clone());
        }
        self.last_change = Some(stored);
    }

    pub fn play<H: VimHost>(
        &mut self,
        host: &mut H,
        name: char,
        count: Option<usize>,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let name = if name == '@' {
            match self.last_macro {
                Some(n) => n,
                None => {
                    self.error("E748: No previously used register");
                    self.fail();
                    return;
                }
            }
        } else {
            name
        };
        let text = if name == ':' {
            match &self.regs.last_ex {
                Some(line) => format!(":{line}\r"),
                None => {
                    self.error("E30: No previous command line");
                    self.fail();
                    return;
                }
            }
        } else if name == '+' || name == '*' {
            match clip.read() {
                Some(t) => t,
                None => {
                    self.fail();
                    return;
                }
            }
        } else {
            match self.regs.get(name) {
                Some(reg) => {
                    if reg.kind == RegisterKind::Line {
                        format!("{}\n", reg.text)
                    } else {
                        reg.text
                    }
                }
                None => {
                    self.fail();
                    return;
                }
            }
        };
        self.last_macro = Some(name);
        let keys = text_keys(&text);
        self.rec.clear();
        for _ in 0..count.unwrap_or(1).max(1) {
            self.feed(host, &keys, opts, clip);
            if self.failed {
                break;
            }
        }
        self.changed = false;
    }

    pub fn start_recording(&mut self, name: char) {
        self.recording = Some((name, Vec::new()));
    }

    pub fn stop_recording(&mut self) {
        let Some((name, mut keys)) = self.recording.take() else {
            return;
        };
        // The `q` that stopped it.
        keys.pop();
        let text: String = keys.into_iter().map(key_text).collect();
        self.regs.set(name, Register::chars(text));
    }
}
