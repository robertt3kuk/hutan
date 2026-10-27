//! The grammar of Normal and Visual mode (`:help normal-index`, `:help visual-index`): the keys
//! of a command in progress are parsed again on every key into a [`Command`], or found to need
//! more keys, a `:` / `/` / `?` line, or to be no command at all.

use crate::key::{Key, Mode};
use crate::register;
use crate::text::Find;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `gj` `gk`: a display row of a soft-wrapped line down / up (`j` `k` for a host that does
    /// not wrap); `g0` `g^` `g$`: the start, first non-blank and end of the display row.
    ScreenDown,
    ScreenUp,
    ScreenLineStart,
    ScreenFirstNonBlank,
    ScreenLineEnd,
    /// `+` (down), `-` (up), `_` (`count - 1` down): to the first non-blank, linewise.
    LineDown,
    LineUp,
    LineHere,
    Word {
        big: bool,
    },
    Back {
        big: bool,
    },
    End {
        big: bool,
    },
    BackEnd {
        big: bool,
    },
    LineStart,
    FirstNonBlank,
    LineEnd,
    LastNonBlank,
    /// `|`: a column.
    Column,
    FileStart,
    FileEnd,
    Find(Find, char),
    RepeatFind {
        reverse: bool,
    },
    Percent,
    Paragraph {
        forward: bool,
    },
    Sentence {
        forward: bool,
    },
    /// `]]` `[[` (`close` false: a `{` in the first column), `][` `[]` (`}`).
    Section {
        forward: bool,
        close: bool,
    },
    /// `'x` (`exact` false: the line) and `` `x ``.
    Mark {
        name: char,
        exact: bool,
    },
    /// `/text` `?text`: a pattern and an optional `/offset`.
    Search {
        forward: bool,
        text: String,
    },
    SearchNext {
        reverse: bool,
    },
    /// `*` `#` (`whole`: `\<word\>`), `g*` `g#`.
    Star {
        forward: bool,
        whole: bool,
    },
}

impl Motion {
    /// What a screen motion (`gj` `gk` `g0` `g^` `g$`) is on a host that does not wrap lines.
    pub fn without_wrap(&self) -> Option<Motion> {
        Some(match self {
            Motion::ScreenDown => Motion::Down,
            Motion::ScreenUp => Motion::Up,
            Motion::ScreenLineStart => Motion::LineStart,
            Motion::ScreenFirstNonBlank => Motion::FirstNonBlank,
            Motion::ScreenLineEnd => Motion::LineEnd,
            _ => return None,
        })
    }

    /// Whether the motion is a jump (`:help jump-motions`): it sets `''` and the jumplist.
    pub fn is_jump(&self) -> bool {
        matches!(
            self,
            Motion::FileStart
                | Motion::FileEnd
                | Motion::Percent
                | Motion::Paragraph { .. }
                | Motion::Sentence { .. }
                | Motion::Section { .. }
                | Motion::Mark { .. }
                | Motion::Search { .. }
                | Motion::SearchNext { .. }
                | Motion::Star { .. }
        )
    }

    /// Whether a delete with this motion always goes to `"1` (`:help quote1`).
    pub fn uses_register_one(&self) -> bool {
        matches!(
            self,
            Motion::Percent
                | Motion::Sentence { .. }
                | Motion::Paragraph { .. }
                | Motion::Mark { exact: true, .. }
                | Motion::Search { .. }
                | Motion::SearchNext { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Delete,
    Change,
    Yank,
    ShiftLeft,
    ShiftRight,
    Indent,
    Tilde,
    Lower,
    Upper,
    Rot13,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Object {
    Word { big: bool },
    Sentence,
    Paragraph,
    Block { open: char, close: char },
    Quote(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Motion(Motion),
    Object {
        object: Object,
        around: bool,
    },
    /// The operator doubled (`dd`, `>>`, `gUU`): `count` lines.
    Lines,
}

/// How an uppercase Visual command widens the selection first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Widen {
    None,
    /// `D X Y C S R` (`D` and `C` in a block: to the end of each line).
    Lines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertAt {
    /// `i`
    Before,
    /// `a`
    After,
    /// `I`
    FirstNonBlank,
    /// `A`
    LineEnd,
    /// `gI`
    LineStart,
    /// `o`
    Below,
    /// `O`
    Above,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Simple {
    Replace(char),
    ToggleCase,
    Insert(InsertAt),
    /// `p` `P` `gp` `gP`.
    Put {
        before: bool,
        cursor_after: bool,
    },
    /// `J` (`spaces`), `gJ`.
    Join {
        spaces: bool,
    },
    Undo,
    Redo,
    /// `v` `V` `⌃V`: start a Visual mode, switch to it, or leave it.
    Visual(Mode),
    /// `gv`.
    Reselect,
    SetMark(char),
    Record(char),
    StopRecord,
    Play(char),
    Repeat,
    RepeatSubstitute,
    /// `⌃O` (-1) and `⌃I` (+1).
    Jump(isize),
    Escape,
    /// `⌃D` `⌃U` `⌃F` `⌃B` `⌃E` `⌃Y`.
    Scroll(Scroll),
    /// `⌃A` (`add`) / `⌃X` on the number at or after the caret; Visual `g⌃A` / `g⌃X` count up
    /// line by line (`progressive`).
    Increment {
        add: bool,
        progressive: bool,
    },
    /// `⌃L`: Vim redraws; nothing to do here.
    Redraw,
    // Visual mode only.
    /// `o`
    SwapEnds,
    /// `O` (the other corner of a block).
    SwapCorner,
    /// `I` `A`.
    VisualInsert {
        append: bool,
    },
    VisualReplace(char),
    VisualPut {
        before: bool,
    },
}

/// How a scroll key moves the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scroll {
    HalfDown,
    HalfUp,
    PageDown,
    PageUp,
    LineDown,
    LineUp,
}

/// The scroll, increment and redraw commands of a ⌃ key (Normal and Visual mode alike).
fn ctrl_simple(c: char) -> Option<Simple> {
    Some(match c {
        'd' => Simple::Scroll(Scroll::HalfDown),
        'u' => Simple::Scroll(Scroll::HalfUp),
        'f' => Simple::Scroll(Scroll::PageDown),
        'b' => Simple::Scroll(Scroll::PageUp),
        'e' => Simple::Scroll(Scroll::LineDown),
        'y' => Simple::Scroll(Scroll::LineUp),
        'a' | 'x' => Simple::Increment {
            add: c == 'a',
            progressive: false,
        },
        'l' => Simple::Redraw,
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Move(Motion),
    Operate(Op, Target),
    /// An operator on the Visual selection.
    VisualOperate(Op, Widen),
    /// `iw`, `a(`, … in Visual mode: the selection becomes (or grows to) the object.
    Object(Object, bool),
    Simple(Simple),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub reg: Option<char>,
    pub count: Option<usize>,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parse {
    /// More keys are needed.
    Incomplete,
    Done(Command),
    /// Escape: the command is dropped, quietly.
    Cancel,
    /// Not a command: Vim beeps.
    Invalid,
    /// A command Vim has and Hutan does not: "Not supported: …".
    Unsupported(String),
    /// The keys go on in a `:` (`':'`) or search (`'/'`, `'?'`) line.
    Line(char),
}

/// What the parser is told besides the keys.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    pub visual: bool,
    pub recording: bool,
    /// The search line typed for a `/` or `?` in the keys, once it is complete.
    pub search: Option<&'a str>,
}

enum Step<T> {
    Need,
    No,
    Yes(T, usize),
    Line(char),
    Unsupported(String),
    Cancel,
}

fn ch(keys: &[Key], i: usize) -> Option<char> {
    match keys.get(i) {
        Some(Key::Char(c)) => Some(*c),
        _ => None,
    }
}

/// A character argument (`f<c>`, `r<c>`, `m<c>`): `None` when the keys stop, `Err` for a key
/// that is not a character (Escape cancels).
fn char_arg(keys: &[Key], i: usize) -> Option<Result<char, bool>> {
    match keys.get(i)? {
        Key::Char(c) => Some(Ok(*c)),
        Key::Escape => Some(Err(true)),
        _ => Some(Err(false)),
    }
}

fn name_of(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match k {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            other => format!("<{other:?}>"),
        })
        .collect()
}

/// A motion at `keys[i]`, and where its keys end.
fn motion(keys: &[Key], i: usize, cx: Context<'_>) -> Step<Motion> {
    let Some(key) = keys.get(i) else {
        return Step::Need;
    };
    let one = |m: Motion| Step::Yes(m, i + 1);
    let c = match key {
        Key::Left | Key::Backspace => return one(Motion::Left),
        Key::Right => return one(Motion::Right),
        Key::Up => return one(Motion::Up),
        Key::Down => return one(Motion::Down),
        Key::Home => return one(Motion::LineStart),
        Key::End => return one(Motion::LineEnd),
        Key::Char(c) => *c,
        _ => return Step::No,
    };
    match c {
        'h' => one(Motion::Left),
        'l' | ' ' => one(Motion::Right),
        'j' => one(Motion::Down),
        'k' => one(Motion::Up),
        '+' => one(Motion::LineDown),
        '-' => one(Motion::LineUp),
        '_' => one(Motion::LineHere),
        'w' | 'W' => one(Motion::Word { big: c == 'W' }),
        'b' | 'B' => one(Motion::Back { big: c == 'B' }),
        'e' | 'E' => one(Motion::End { big: c == 'E' }),
        '0' => one(Motion::LineStart),
        '^' => one(Motion::FirstNonBlank),
        '$' => one(Motion::LineEnd),
        '|' => one(Motion::Column),
        'G' => one(Motion::FileEnd),
        ';' => one(Motion::RepeatFind { reverse: false }),
        ',' => one(Motion::RepeatFind { reverse: true }),
        '%' => one(Motion::Percent),
        '(' | ')' => one(Motion::Sentence { forward: c == ')' }),
        '{' | '}' => one(Motion::Paragraph { forward: c == '}' }),
        'n' | 'N' => one(Motion::SearchNext { reverse: c == 'N' }),
        '*' | '#' => one(Motion::Star {
            forward: c == '*',
            whole: true,
        }),
        'f' | 't' | 'F' | 'T' => match char_arg(keys, i + 1) {
            None => Step::Need,
            Some(Err(true)) => Step::Cancel,
            Some(Err(false)) => Step::Unsupported(name_of(&keys[i..i + 2])),
            Some(Ok(target)) => Step::Yes(
                Motion::Find(
                    Find {
                        forward: c == 'f' || c == 't',
                        till: c == 't' || c == 'T',
                    },
                    target,
                ),
                i + 2,
            ),
        },
        '\'' | '`' => match char_arg(keys, i + 1) {
            None => Step::Need,
            Some(Err(true)) => Step::Cancel,
            Some(Err(false)) => Step::Unsupported(name_of(&keys[i..i + 2])),
            Some(Ok(name)) if crate::marks::readable(name) => Step::Yes(
                Motion::Mark {
                    name,
                    exact: c == '`',
                },
                i + 2,
            ),
            Some(Ok(name)) => Step::Unsupported(format!("{c}{name}")),
        },
        '[' | ']' => match ch(keys, i + 1) {
            None if keys.get(i + 1).is_none() => Step::Need,
            Some(n @ ('[' | ']')) => Step::Yes(
                Motion::Section {
                    forward: c == ']',
                    close: n != c,
                },
                i + 2,
            ),
            Some(_) if keys.get(i + 1) == Some(&Key::Escape) => Step::Cancel,
            _ if keys.get(i + 1) == Some(&Key::Escape) => Step::Cancel,
            _ => Step::Unsupported(name_of(&keys[i..i + 2])),
        },
        'g' => match keys.get(i + 1) {
            None => Step::Need,
            Some(Key::Escape) => Step::Cancel,
            Some(Key::Char(n)) => match n {
                'g' => Step::Yes(Motion::FileStart, i + 2),
                'e' | 'E' => Step::Yes(Motion::BackEnd { big: *n == 'E' }, i + 2),
                '_' => Step::Yes(Motion::LastNonBlank, i + 2),
                'j' => Step::Yes(Motion::ScreenDown, i + 2),
                'k' => Step::Yes(Motion::ScreenUp, i + 2),
                '0' => Step::Yes(Motion::ScreenLineStart, i + 2),
                '^' => Step::Yes(Motion::ScreenFirstNonBlank, i + 2),
                '$' => Step::Yes(Motion::ScreenLineEnd, i + 2),
                '*' | '#' => Step::Yes(
                    Motion::Star {
                        forward: *n == '*',
                        whole: false,
                    },
                    i + 2,
                ),
                _ => Step::No,
            },
            Some(_) => Step::No,
        },
        '/' | '?' => match cx.search {
            Some(text) => Step::Yes(
                Motion::Search {
                    forward: c == '/',
                    text: text.to_owned(),
                },
                i + 1,
            ),
            None => Step::Line(c),
        },
        'H' | 'M' | 'L' => Step::Unsupported(c.to_string()),
        _ => Step::No,
    }
}

/// An operator at `keys[i]` (`d`, `g~`, …) and where its keys end.
fn operator(keys: &[Key], i: usize, visual: bool) -> Step<Op> {
    let Some(c) = ch(keys, i) else {
        return if keys.get(i).is_none() {
            Step::Need
        } else {
            Step::No
        };
    };
    let one = |op| Step::Yes(op, i + 1);
    match c {
        'd' => one(Op::Delete),
        'c' => one(Op::Change),
        'y' => one(Op::Yank),
        '<' => one(Op::ShiftLeft),
        '>' => one(Op::ShiftRight),
        '=' => one(Op::Indent),
        'x' if visual => one(Op::Delete),
        's' if visual => one(Op::Change),
        '~' if visual => one(Op::Tilde),
        'u' if visual => one(Op::Lower),
        'U' if visual => one(Op::Upper),
        'g' => match ch(keys, i + 1) {
            None if keys.get(i + 1).is_none() => Step::Need,
            Some('~') => Step::Yes(Op::Tilde, i + 2),
            Some('u') => Step::Yes(Op::Lower, i + 2),
            Some('U') => Step::Yes(Op::Upper, i + 2),
            Some('?') => Step::Yes(Op::Rot13, i + 2),
            _ => Step::No,
        },
        _ => Step::No,
    }
}

/// `i`/`a` and an object key.
fn object(keys: &[Key], i: usize) -> Step<(Object, bool)> {
    let around = match ch(keys, i) {
        Some('i') => false,
        Some('a') => true,
        _ => return Step::No,
    };
    let object = match keys.get(i + 1) {
        None => return Step::Need,
        Some(Key::Escape) => return Step::Cancel,
        Some(Key::Char(c)) => match c {
            'w' | 'W' => Object::Word { big: *c == 'W' },
            's' => Object::Sentence,
            'p' => Object::Paragraph,
            '(' | ')' | 'b' => Object::Block {
                open: '(',
                close: ')',
            },
            '{' | '}' | 'B' => Object::Block {
                open: '{',
                close: '}',
            },
            '[' | ']' => Object::Block {
                open: '[',
                close: ']',
            },
            '<' | '>' => Object::Block {
                open: '<',
                close: '>',
            },
            '"' | '\'' | '`' => Object::Quote(*c),
            't' => return Step::Unsupported(format!("{}t", if around { 'a' } else { 'i' })),
            _ => return Step::Unsupported(name_of(&keys[i..i + 2])),
        },
        Some(_) => return Step::Unsupported(name_of(&keys[i..i + 2])),
    };
    Step::Yes((object, around), i + 2)
}

/// Digits at `keys[i]`: the count and where it ends. A `0` cannot start a count.
fn count(keys: &[Key], mut i: usize, mut count: Option<usize>) -> (Option<usize>, usize) {
    while let Some(d) = ch(keys, i).and_then(|c| c.to_digit(10)) {
        if d == 0 && count.is_none() {
            break;
        }
        count = Some(
            count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(d as usize)
                .min(99_999),
        );
        i += 1;
    }
    (count, i)
}

/// Parses the keys of a command in progress.
pub(crate) fn parse(keys: &[Key], cx: Context<'_>) -> Parse {
    // Registers and the count, in any order.
    let mut i = 0;
    let mut reg = None;
    let mut n = None;
    loop {
        let (c, next) = count(keys, i, n);
        n = c;
        i = next;
        if ch(keys, i) == Some('"') {
            match keys.get(i + 1) {
                None => return Parse::Incomplete,
                Some(Key::Escape) => return Parse::Cancel,
                Some(Key::Char(r)) if register::valid_name(*r) => {
                    reg = Some(*r);
                    i += 2;
                }
                Some(_) => return Parse::Invalid,
            }
            continue;
        }
        break;
    }
    let Some(key) = keys.get(i) else {
        return Parse::Incomplete;
    };
    let done = |action: Action| {
        Parse::Done(Command {
            reg,
            count: n,
            action,
        })
    };
    if *key == Key::Escape {
        return if cx.visual && i == 0 {
            done(Action::Simple(Simple::Escape))
        } else {
            Parse::Cancel
        };
    }
    // Motions first: they are the same in both modes.
    match motion(keys, i, cx) {
        Step::Need => return Parse::Incomplete,
        Step::Yes(m, end) if end == keys.len() => return done(Action::Move(m)),
        Step::Yes(..) => return Parse::Invalid,
        Step::Line(c) => return Parse::Line(c),
        Step::Unsupported(s) => return Parse::Unsupported(s),
        Step::Cancel => return Parse::Cancel,
        Step::No => {}
    }
    if cx.visual {
        return parse_visual(keys, i, reg, n);
    }
    // An operator and what it acts on.
    match operator(keys, i, false) {
        Step::Need => return Parse::Incomplete,
        Step::Yes(op, after) => {
            let (n2, j) = count(keys, after, None);
            let total = match (n, n2) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1)).min(99_999)),
            };
            let Some(next) = keys.get(j) else {
                return Parse::Incomplete;
            };
            if *next == Key::Escape {
                return Parse::Cancel;
            }
            let op_keys = &keys[i..after];
            // Doubled: `dd`, `g~~`, `g~g~`, `gUU`.
            let doubled = keys.get(j..j + op_keys.len()) == Some(op_keys)
                || (op_keys.len() == 2 && keys.get(j) == op_keys.get(1));
            if doubled {
                let end = if keys.get(j..j + op_keys.len()) == Some(op_keys) {
                    j + op_keys.len()
                } else {
                    j + 1
                };
                if op_keys.len() == 2 && keys.len() == j + 1 && keys[j] == op_keys[0] {
                    return Parse::Incomplete;
                }
                if end == keys.len() {
                    return Parse::Done(Command {
                        reg,
                        count: total,
                        action: Action::Operate(op, Target::Lines),
                    });
                }
            }
            if op_keys.len() == 2 && keys.len() == j + 1 && keys[j] == op_keys[0] {
                // `g~g` on the way to `g~g~`, or `g~g…` a `g` motion.
                return Parse::Incomplete;
            }
            let target = match object(keys, j) {
                Step::Need => return Parse::Incomplete,
                Step::Yes((object, around), end) if end == keys.len() => {
                    Target::Object { object, around }
                }
                Step::Yes(..) => return Parse::Invalid,
                Step::Unsupported(s) => return Parse::Unsupported(s),
                Step::Cancel => return Parse::Cancel,
                Step::Line(_) => return Parse::Invalid,
                Step::No => match motion(keys, j, cx) {
                    Step::Need => return Parse::Incomplete,
                    Step::Yes(m, end) if end == keys.len() => Target::Motion(m),
                    Step::Yes(..) => return Parse::Invalid,
                    Step::Line(c) => return Parse::Line(c),
                    Step::Unsupported(s) => return Parse::Unsupported(s),
                    Step::Cancel => return Parse::Cancel,
                    Step::No => {
                        return match ch(keys, j) {
                            Some('v' | 'V') => Parse::Unsupported(name_of(&keys[i..=j])),
                            _ if keys[j] == Key::Ctrl('v') => {
                                Parse::Unsupported(name_of(&keys[i..=j]))
                            }
                            _ => Parse::Invalid,
                        };
                    }
                },
            };
            return Parse::Done(Command {
                reg,
                count: total,
                action: Action::Operate(op, target),
            });
        }
        _ => {}
    }
    let simple = |s: Simple, len: usize| {
        if i + len == keys.len() {
            done(Action::Simple(s))
        } else {
            Parse::Invalid
        }
    };
    let operate = |op: Op, target: Target| done(Action::Operate(op, target));
    match key {
        Key::Ctrl('r') => return simple(Simple::Redo, 1),
        Key::Ctrl('v') => return simple(Simple::Visual(Mode::VisualBlock), 1),
        Key::Ctrl('o') => return simple(Simple::Jump(-1), 1),
        Key::Ctrl('i') | Key::Tab => return simple(Simple::Jump(1), 1),
        Key::Delete => return operate(Op::Delete, Target::Motion(Motion::Right)),
        Key::Ctrl(c) => {
            return match ctrl_simple(*c) {
                Some(s) => simple(s, 1),
                None => Parse::Unsupported(format!("<C-{c}>")),
            };
        }
        Key::Char(_) => {}
        _ => return Parse::Invalid,
    }
    let Some(c) = ch(keys, i) else {
        return Parse::Invalid;
    };
    let arg = |k: usize| char_arg(keys, i + k);
    match c {
        'x' => operate(Op::Delete, Target::Motion(Motion::Right)),
        'X' => operate(Op::Delete, Target::Motion(Motion::Left)),
        'D' => operate(Op::Delete, Target::Motion(Motion::LineEnd)),
        'C' => operate(Op::Change, Target::Motion(Motion::LineEnd)),
        's' => operate(Op::Change, Target::Motion(Motion::Right)),
        'S' => operate(Op::Change, Target::Lines),
        'Y' => operate(Op::Yank, Target::Lines),
        'r' => match arg(1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(r)) => simple(Simple::Replace(r), 2),
            Some(Err(false)) if keys[i + 1] == Key::Enter => simple(Simple::Replace('\n'), 2),
            Some(Err(false)) => Parse::Invalid,
        },
        '~' => simple(Simple::ToggleCase, 1),
        'i' => simple(Simple::Insert(InsertAt::Before), 1),
        'a' => simple(Simple::Insert(InsertAt::After), 1),
        'I' => simple(Simple::Insert(InsertAt::FirstNonBlank), 1),
        'A' => simple(Simple::Insert(InsertAt::LineEnd), 1),
        'o' => simple(Simple::Insert(InsertAt::Below), 1),
        'O' => simple(Simple::Insert(InsertAt::Above), 1),
        'p' | 'P' => simple(
            Simple::Put {
                before: c == 'P',
                cursor_after: false,
            },
            1,
        ),
        'J' => simple(Simple::Join { spaces: true }, 1),
        'u' => simple(Simple::Undo, 1),
        'v' => simple(Simple::Visual(Mode::Visual), 1),
        'V' => simple(Simple::Visual(Mode::VisualLine), 1),
        'm' => match arg(1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(m))
                if m.is_ascii_alphabetic() || matches!(m, '\'' | '`' | '[' | ']' | '<' | '>') =>
            {
                simple(Simple::SetMark(m), 2)
            }
            Some(_) => Parse::Invalid,
        },
        'q' if cx.recording => simple(Simple::StopRecord, 1),
        'q' => match arg(1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(r)) if r.is_ascii_alphanumeric() || r == '"' => simple(Simple::Record(r), 2),
            Some(Ok(':' | '/' | '?')) => Parse::Unsupported(format!("q{}", keys_char(keys, i + 1))),
            Some(_) => Parse::Invalid,
        },
        '@' => match arg(1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(r))
                if r.is_ascii_alphanumeric()
                    || matches!(r, '@' | ':' | '"' | '-' | '.' | '+' | '*') =>
            {
                simple(Simple::Play(r), 2)
            }
            Some(_) => Parse::Invalid,
        },
        '.' => simple(Simple::Repeat, 1),
        '&' => simple(Simple::RepeatSubstitute, 1),
        ':' => {
            if i + 1 == keys.len() {
                Parse::Line(':')
            } else {
                Parse::Invalid
            }
        }
        'g' => match keys.get(i + 1) {
            None => Parse::Incomplete,
            Some(Key::Escape) => Parse::Cancel,
            Some(Key::Char(n)) => match n {
                'p' | 'P' => simple(
                    Simple::Put {
                        before: *n == 'P',
                        cursor_after: true,
                    },
                    2,
                ),
                'J' => simple(Simple::Join { spaces: false }, 2),
                'I' => simple(Simple::Insert(InsertAt::LineStart), 2),
                'v' => simple(Simple::Reselect, 2),
                _ => Parse::Unsupported(format!("g{n}")),
            },
            Some(_) => Parse::Invalid,
        },
        'Z' | 'z' => match keys.get(i + 1) {
            None => Parse::Incomplete,
            Some(Key::Escape) => Parse::Cancel,
            Some(_) => Parse::Unsupported(name_of(&keys[i..i + 2])),
        },
        '!' | 'K' | 'Q' | 'R' | 'U' => Parse::Unsupported(c.to_string()),
        _ => Parse::Invalid,
    }
}

fn keys_char(keys: &[Key], i: usize) -> char {
    ch(keys, i).unwrap_or('?')
}

fn parse_visual(keys: &[Key], i: usize, reg: Option<char>, n: Option<usize>) -> Parse {
    let done = |action: Action| {
        Parse::Done(Command {
            reg,
            count: n,
            action,
        })
    };
    let key = keys[i];
    let simple = |s: Simple, len: usize| {
        if i + len == keys.len() {
            done(Action::Simple(s))
        } else {
            Parse::Invalid
        }
    };
    match operator(keys, i, true) {
        Step::Need => return Parse::Incomplete,
        Step::Yes(op, end) if end == keys.len() => {
            return done(Action::VisualOperate(op, Widen::None));
        }
        Step::Yes(..) => return Parse::Invalid,
        _ => {}
    }
    match object(keys, i) {
        Step::Need => return Parse::Incomplete,
        Step::Yes((object, around), end) if end == keys.len() => {
            return done(Action::Object(object, around));
        }
        Step::Yes(..) => return Parse::Invalid,
        Step::Unsupported(s) => return Parse::Unsupported(s),
        Step::Cancel => return Parse::Cancel,
        _ => {}
    }
    match key {
        Key::Ctrl('v') => return simple(Simple::Visual(Mode::VisualBlock), 1),
        Key::Ctrl(c) => {
            return match ctrl_simple(c) {
                Some(s) => simple(s, 1),
                None => Parse::Unsupported(format!("<C-{c}>")),
            };
        }
        Key::Delete => return done(Action::VisualOperate(Op::Delete, Widen::None)),
        Key::Char(_) => {}
        _ => return Parse::Invalid,
    }
    let Some(c) = ch(keys, i) else {
        return Parse::Invalid;
    };
    match c {
        'D' | 'X' => done(Action::VisualOperate(Op::Delete, Widen::Lines)),
        'Y' => done(Action::VisualOperate(Op::Yank, Widen::Lines)),
        'C' | 'S' | 'R' => done(Action::VisualOperate(Op::Change, Widen::Lines)),
        'J' => simple(Simple::Join { spaces: true }, 1),
        'v' => simple(Simple::Visual(Mode::Visual), 1),
        'V' => simple(Simple::Visual(Mode::VisualLine), 1),
        'o' => simple(Simple::SwapEnds, 1),
        'O' => simple(Simple::SwapCorner, 1),
        'I' | 'A' => simple(Simple::VisualInsert { append: c == 'A' }, 1),
        'p' | 'P' => simple(Simple::VisualPut { before: c == 'P' }, 1),
        'r' => match char_arg(keys, i + 1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(r)) => simple(Simple::VisualReplace(r), 2),
            Some(Err(false)) => Parse::Invalid,
        },
        'm' => match char_arg(keys, i + 1) {
            None => Parse::Incomplete,
            Some(Err(true)) => Parse::Cancel,
            Some(Ok(m)) if m.is_ascii_alphabetic() => simple(Simple::SetMark(m), 2),
            Some(_) => Parse::Invalid,
        },
        ':' => {
            if i + 1 == keys.len() {
                Parse::Line(':')
            } else {
                Parse::Invalid
            }
        }
        'g' => match keys.get(i + 1) {
            None => Parse::Incomplete,
            Some(Key::Escape) => Parse::Cancel,
            Some(Key::Char('J')) => simple(Simple::Join { spaces: false }, 2),
            Some(Key::Char('v')) => simple(Simple::Reselect, 2),
            Some(Key::Ctrl(c @ ('a' | 'x'))) => simple(
                Simple::Increment {
                    add: *c == 'a',
                    progressive: true,
                },
                2,
            ),
            Some(Key::Char(n)) => Parse::Unsupported(format!("g{n}")),
            Some(_) => Parse::Invalid,
        },
        '!' | 'K' | 'Z' | 'z' | 'H' | 'M' | 'L' => Parse::Unsupported(c.to_string()),
        _ => Parse::Invalid,
    }
}
