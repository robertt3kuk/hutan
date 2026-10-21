//! Vim's search patterns (`:help pattern`), translated to the `regex` crate, and the
//! replacement strings of `:s` (`:help sub-replace-special`).
//!
//! What translates: the four magic levels (`\v` `\m` `\M` `\V`, magic by default), `\c` / `\C`,
//! `^` `$` `.` `*` `[…]` (with `[:alpha:]` classes and `\e \t \r \n \\ \]` inside), `~` (the
//! last replacement), `\( \) \%( \|`, `\+ \= \?`, `\{n,m}` with `\{-…}` for the lazy forms,
//! `\< \>`, `\zs \ze`, and the classes `\s \S \d \D \w \W \a \A \l \L \u \U \x \X \o \O \h
//! \H \k \K \i \I \f \F \p \P`. Look-around (`\@=` …), back references (`\1` in a pattern),
//! `\_x` and `\%` items other than `\%(` answer "Not supported". Patterns match within one line.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::ex::ExError;

/// `:set ignorecase` and `:set smartcase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaseOptions {
    pub ignore_case: bool,
    pub smart_case: bool,
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    regex: Regex,
    /// The regex group of each of the pattern's `\(…\)`, in order.
    groups: Vec<usize>,
    /// The groups `\zs` and `\ze` became (empty groups that mark a position).
    zs: Option<usize>,
    ze: Option<usize>,
}

/// One match in a line: where it is, and the text of its groups (`0` the whole match).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub range: Range<usize>,
    groups: Vec<Option<Range<usize>>>,
}

impl Found {
    /// The byte range of group `n` (`0` the match) in the line it was found in.
    pub fn group(&self, n: usize) -> Option<Range<usize>> {
        if n == 0 {
            Some(self.range.clone())
        } else {
            self.groups.get(n - 1).cloned().flatten()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Magic {
    Very,
    On,
    Off,
    VeryOff,
}

fn unsupported(what: &str) -> ExError {
    ExError(format!("Not supported: {what} in a pattern"))
}

/// Whether `c` is special in `magic` when written plain (`escaped` false) or after a backslash.
fn special(c: char, escaped: bool, magic: Magic) -> bool {
    const MAGIC: &str = "^$.*[~";
    const BACKSLASH: &str = "()|+=?{<>@%";
    match magic {
        Magic::Very => !escaped && (MAGIC.contains(c) || BACKSLASH.contains(c)),
        Magic::On => {
            if escaped {
                BACKSLASH.contains(c)
            } else {
                MAGIC.contains(c)
            }
        }
        Magic::Off => {
            if escaped {
                BACKSLASH.contains(c) || ".*[~".contains(c)
            } else {
                "^$".contains(c)
            }
        }
        Magic::VeryOff => {
            if escaped {
                BACKSLASH.contains(c) || ".*[~".contains(c)
            } else {
                "^$".contains(c)
            }
        }
    }
}

struct Translation {
    source: String,
    groups: Vec<usize>,
    zs: Option<usize>,
    ze: Option<usize>,
    /// `\c` (true) or `\C` (false).
    case: Option<bool>,
}

/// Whether the pattern has an uppercase letter that is not part of an escape (`'smartcase'`).
fn has_uppercase(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

fn translate(pattern: &str, last_replacement: Option<&str>) -> Result<Translation, ExError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut magic = Magic::On;
    let mut groups = Vec::new();
    let mut regex_groups = 0usize;
    let mut zs = None;
    let mut ze = None;
    let mut case = None;
    // Whether the next item starts a branch (`^` and `*` are literal elsewhere).
    let mut branch_start = true;
    let mut i = 0;
    while i < chars.len() {
        let mut c = chars[i];
        let mut escaped = false;
        i += 1;
        if c == '\\' {
            let Some(&n) = chars.get(i) else {
                out.push_str("\\\\");
                break;
            };
            i += 1;
            match n {
                'v' => {
                    magic = Magic::Very;
                    continue;
                }
                'm' => {
                    magic = Magic::On;
                    continue;
                }
                'M' => {
                    magic = Magic::Off;
                    continue;
                }
                'V' => {
                    magic = Magic::VeryOff;
                    continue;
                }
                'c' => {
                    case = Some(true);
                    continue;
                }
                'C' => {
                    case = Some(false);
                    continue;
                }
                'z' => {
                    let which = chars.get(i).copied();
                    i += 1;
                    regex_groups += 1;
                    match which {
                        Some('s') => zs = Some(regex_groups),
                        Some('e') => ze = Some(regex_groups),
                        _ => return Err(unsupported("\\z")),
                    }
                    out.push_str("()");
                    continue;
                }
                '_' => return Err(unsupported("\\_")),
                '1'..='9' => return Err(unsupported("a back reference")),
                _ => {}
            }
            if let Some(class) = class_of(n) {
                out.push_str(class);
                branch_start = false;
                continue;
            }
            if n.is_alphanumeric() {
                return Err(unsupported(&format!("\\{n}")));
            }
            c = n;
            escaped = true;
        }
        if !special(c, escaped, magic) {
            push_literal(&mut out, c);
            branch_start = false;
            continue;
        }
        match c {
            '^' => {
                if branch_start {
                    out.push('^');
                } else {
                    push_literal(&mut out, '^');
                    branch_start = false;
                }
            }
            '$' => {
                if ends_branch(&chars[i..], magic) {
                    out.push('$');
                } else {
                    push_literal(&mut out, '$');
                }
                branch_start = false;
            }
            '.' => {
                out.push('.');
                branch_start = false;
            }
            '*' | '+' | '=' | '?' => {
                if branch_start {
                    push_literal(&mut out, c);
                } else {
                    out.push(if c == '=' { '?' } else { c });
                }
                branch_start = false;
            }
            '[' => {
                match collection(&chars[i..]) {
                    Some((class, used)) => {
                        out.push_str(&class);
                        i += used;
                    }
                    None => push_literal(&mut out, '['),
                }
                branch_start = false;
            }
            '~' => {
                for r in last_replacement.unwrap_or("").chars() {
                    push_literal(&mut out, r);
                }
                branch_start = false;
            }
            '(' => {
                regex_groups += 1;
                groups.push(regex_groups);
                out.push('(');
                branch_start = true;
            }
            ')' => {
                out.push(')');
                branch_start = false;
            }
            '%' => {
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    out.push_str("(?:");
                    branch_start = true;
                } else {
                    return Err(unsupported("\\%"));
                }
            }
            '|' => {
                out.push('|');
                branch_start = true;
            }
            '{' => {
                let close = chars[i..]
                    .iter()
                    .position(|&c| c == '}')
                    .ok_or_else(|| ExError("E60: Missing }".into()))?;
                let mut body: String = chars[i..i + close].iter().collect();
                i += close + 1;
                if let Some(b) = body.strip_suffix('\\') {
                    body = b.to_owned();
                }
                let lazy = body.starts_with('-');
                let body = body.trim_start_matches('-');
                let counts = if body.is_empty() {
                    "*".to_owned()
                } else if let Some(max) = body.strip_prefix(',') {
                    format!("{{0,{max}}}")
                } else {
                    format!("{{{body}}}")
                };
                if body.chars().any(|c| !c.is_ascii_digit() && c != ',') {
                    return Err(ExError("E554: Syntax error in \\{...}".into()));
                }
                out.push_str(&counts);
                if lazy {
                    out.push('?');
                }
                branch_start = false;
            }
            '<' => {
                out.push_str("\\b{start}");
                branch_start = false;
            }
            '>' => {
                out.push_str("\\b{end}");
                branch_start = false;
            }
            '@' => return Err(unsupported("\\@")),
            other => {
                push_literal(&mut out, other);
                branch_start = false;
            }
        }
    }
    Ok(Translation {
        source: out,
        groups,
        zs,
        ze,
        case,
    })
}

/// Whether what follows a `$` ends the branch: the end, `\|` or `\)` (`|` `)` very magic).
fn ends_branch(rest: &[char], magic: Magic) -> bool {
    match rest {
        [] => true,
        ['\\', c, ..] if magic != Magic::Very => *c == '|' || *c == ')',
        [c, ..] if magic == Magic::Very => *c == '|' || *c == ')',
        _ => false,
    }
}

fn push_literal(out: &mut String, c: char) {
    let mut buf = [0; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut buf)));
}

fn class_of(c: char) -> Option<&'static str> {
    Some(match c {
        's' => "[ \\t]",
        'S' => "[^ \\t]",
        'd' => "[0-9]",
        'D' => "[^0-9]",
        'w' => "[0-9A-Za-z_]",
        'W' => "[^0-9A-Za-z_]",
        'a' => "[A-Za-z]",
        'A' => "[^A-Za-z]",
        'l' => "[a-z]",
        'L' => "[^a-z]",
        'u' => "[A-Z]",
        'U' => "[^A-Z]",
        'x' => "[0-9A-Fa-f]",
        'X' => "[^0-9A-Fa-f]",
        'o' => "[0-7]",
        'O' => "[^0-7]",
        'h' => "[A-Za-z_]",
        'H' => "[^A-Za-z_]",
        'k' | 'i' => "[0-9A-Za-z_\\u{C0}-\\u{FF}]",
        'K' | 'I' => "[A-Za-z_\\u{C0}-\\u{FF}]",
        'f' => "[0-9A-Za-z/.\\-_+,#$%~=]",
        'F' => "[A-Za-z/.\\-_+,#$%~=]",
        'p' => "[^\\x00-\\x1f\\x7f]",
        'P' => "[^\\x00-\\x1f\\x7f0-9]",
        'e' => "\\x1b",
        't' => "\\t",
        'r' => "\\r",
        'n' => "\\n",
        'b' => "\\x08",
        _ => return None,
    })
}

/// A `[…]` collection (after its `[`): the regex class and how many characters it took;
/// `None` when it has no `]` (the `[` is then literal).
fn collection(rest: &[char]) -> Option<(String, usize)> {
    let mut out = String::from("[");
    let mut i = 0;
    if rest.first() == Some(&'^') {
        out.push('^');
        i += 1;
    }
    // A `]` first is literal.
    if rest.get(i) == Some(&']') {
        out.push_str("\\]");
        i += 1;
    }
    while i < rest.len() {
        let c = rest[i];
        match c {
            ']' => {
                out.push(']');
                return Some((out, i + 1));
            }
            '[' if rest.get(i + 1) == Some(&':') => {
                let end = rest[i + 2..].iter().position(|&c| c == ':')?;
                let name: String = rest[i + 2..i + 2 + end].iter().collect();
                if rest.get(i + 2 + end + 1) != Some(&']') {
                    out.push_str("\\[");
                    i += 1;
                    continue;
                }
                out.push_str(&format!("[:{name}:]"));
                i += end + 4;
            }
            '\\' => {
                let n = rest.get(i + 1).copied()?;
                match n {
                    'e' => out.push_str("\\x1b"),
                    't' => out.push_str("\\t"),
                    'r' => out.push_str("\\r"),
                    'n' => out.push_str("\\n"),
                    'b' => out.push_str("\\x08"),
                    '\\' | ']' | '^' | '-' => {
                        out.push('\\');
                        out.push(n);
                    }
                    other => {
                        // Vim keeps the backslash of an unknown escape.
                        out.push_str("\\\\");
                        push_class_char(&mut out, other);
                    }
                }
                i += 2;
            }
            '-' => {
                out.push('-');
                i += 1;
            }
            other => {
                push_class_char(&mut out, other);
                i += 1;
            }
        }
    }
    None
}

/// A character inside a class, escaped where the `regex` crate gives it a meaning Vim does not
/// (nested classes, set operations).
fn push_class_char(out: &mut String, c: char) {
    if matches!(c, '[' | '&' | '~' | '^' | '\\') {
        out.push('\\');
    }
    out.push(c);
}

impl Pattern {
    /// Compiles `pattern`. `ignore_case` set forces the case (`:s///i`, `*` with
    /// `'ignorecase'`); else `'ignorecase'` with `'smartcase'` decide; `\c` and `\C` in the
    /// pattern win over both. `last_replacement` is what `~` matches.
    pub fn new(
        pattern: &str,
        ignore_case: Option<bool>,
        case: CaseOptions,
        last_replacement: Option<&str>,
    ) -> Result<Self, ExError> {
        let t = translate(pattern, last_replacement)?;
        let insensitive = t
            .case
            .or(ignore_case)
            .unwrap_or_else(|| case.ignore_case && !(case.smart_case && has_uppercase(pattern)));
        let regex = RegexBuilder::new(&t.source)
            .case_insensitive(insensitive)
            .size_limit(8 << 20)
            .build()
            .map_err(|e| {
                let text = e.to_string();
                let last = text.lines().last().unwrap_or(&text).trim().to_owned();
                ExError(format!("E486: Bad pattern: {last}"))
            })?;
        Ok(Self {
            regex,
            groups: t.groups,
            zs: t.zs,
            ze: t.ze,
        })
    }

    /// Whether `line` has a match.
    pub fn is_match(&self, line: &str) -> bool {
        self.regex.is_match(line)
    }

    /// The first match whose regex match starts at or after byte `from` of `line`.
    pub fn find_at(&self, line: &str, from: usize) -> Option<Found> {
        if from > line.len() {
            return None;
        }
        let caps = self.regex.captures_at(line, from)?;
        let whole = caps.get(0)?;
        let start = self
            .zs
            .and_then(|g| caps.get(g))
            .map_or(whole.start(), |m| m.start());
        let end = self
            .ze
            .and_then(|g| caps.get(g))
            .map_or(whole.end(), |m| m.start())
            .max(start);
        let groups = self
            .groups
            .iter()
            .map(|&g| caps.get(g).map(|m| m.range()))
            .collect();
        Some(Found {
            range: start..end,
            groups,
        })
    }

    /// The matches of `line`, left to right and not overlapping (an empty match steps one
    /// character on), as Vim walks them for `:s///g` and for searching.
    pub fn matches(&self, line: &str) -> Vec<Found> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(found) = self.find_at(line, from) {
            let next = if found.range.is_empty() {
                let at = found.range.end;
                match line[at..].chars().next() {
                    Some(c) => at + c.len_utf8(),
                    None => line.len() + 1,
                }
            } else {
                found.range.end
            };
            out.push(found);
            if next > line.len() {
                break;
            }
            from = next;
        }
        out
    }

    /// The edits `:s` makes in `line` (byte ranges of the line and their new text): the first
    /// match, or every one with `global`. `replacement` is already expanded for `~`.
    pub fn replacements(
        &self,
        line: &str,
        replacement: &str,
        global: bool,
    ) -> Vec<(Range<usize>, String)> {
        let mut found = self.matches(line);
        if !global {
            found.truncate(1);
        }
        found
            .into_iter()
            .map(|f| {
                let text = expand(replacement, line, &f);
                (f.range, text)
            })
            .collect()
    }
}

/// `replacement` with `~` replaced by the previous replacement (`:help s~`), unescaped `~`
/// only. The result is the next "previous replacement".
pub fn with_previous(replacement: &str, previous: Option<&str>) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push('\\');
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '~' => out.push_str(previous.unwrap_or("")),
            c => out.push(c),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseChange {
    None,
    Upper,
    Lower,
}

/// The text one match becomes (`:help sub-replace-special`): `&` and `\0` the match, `\1`–`\9`
/// its groups, `\u \l` the next character's case, `\U \L` until `\E` / `\e`, `\r` and `\n` a
/// line break, `\t` a tab, `\\` `\&` literally.
pub fn expand(replacement: &str, line: &str, found: &Found) -> String {
    let mut out = String::new();
    let mut one = CaseChange::None;
    let mut all = CaseChange::None;
    let push = |out: &mut String, s: &str, one: &mut CaseChange, all: CaseChange| {
        for c in s.chars() {
            let case = if *one != CaseChange::None {
                std::mem::replace(one, CaseChange::None)
            } else {
                all
            };
            match case {
                CaseChange::Upper => out.extend(c.to_uppercase()),
                CaseChange::Lower => out.extend(c.to_lowercase()),
                CaseChange::None => out.push(c),
            }
        }
    };
    let group = |n: usize| found.group(n).map_or("", |r| &line[r]);
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => push(&mut out, group(0), &mut one, all),
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => {
                    push(&mut out, group(d as usize - '0' as usize), &mut one, all);
                }
                Some('u') => one = CaseChange::Upper,
                Some('l') => one = CaseChange::Lower,
                Some('U') => all = CaseChange::Upper,
                Some('L') => all = CaseChange::Lower,
                Some('E' | 'e') => all = CaseChange::None,
                Some('r' | 'n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => push(&mut out, other.encode_utf8(&mut [0; 4]), &mut one, all),
                None => out.push('\\'),
            },
            '\r' => out.push('\n'),
            other => push(&mut out, other.encode_utf8(&mut [0; 4]), &mut one, all),
        }
    }
    out
}
