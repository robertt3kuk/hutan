//! Vim's `:` line for the code view (slice E-8): a small parser of the ex commands Hutan runs
//! (`:w :q :wq :x :e :sp :vs :b :ls :s :g :v :normal :noh :set :term` and `:N`), line ranges
//! (`%`, `.`, `$`, `N`, `'<`, `'>`, with `+N`/`-N` offsets), and the text work of `:s` and
//! `:g` over a snapshot of the rope (it runs on a background thread). Anything else is
//! answered with "Not supported: …", never swallowed.
//!
//! Patterns are Rust `regex` syntax (as find's regex mode), not Vim's: `\<` and `\>` are taken
//! as word boundaries; `&`, `\0`–`\9`, `\r`/`\n` and `\\` work in a replacement.

use std::ops::Range;

use hutan_buffer::{Edit, Text};
use regex::{Regex, RegexBuilder};

/// One end of a line range, before it is resolved against the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// `N`: a 1-based line number.
    Line(usize),
    /// `.`: the caret's line.
    Current,
    /// `$`: the last line.
    Last,
    /// `'<`: the first line of the last Visual selection.
    VisualStart,
    /// `'>`: its last line.
    VisualEnd,
}

/// An address and its `+N` / `-N` offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRef {
    pub address: Address,
    pub offset: i64,
}

/// The lines a command acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSpec {
    /// `%`: every line.
    All,
    One(LineRef),
    Two(LineRef, LineRef),
}

/// What `:set` is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOption {
    /// `number` / `nonumber` / `invnumber` (`nu`).
    Number(Toggle),
    /// `ignorecase` (`ic`).
    IgnoreCase(Toggle),
    /// `smartcase` (`scs`).
    SmartCase(Toggle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    Invert,
    /// `name?`: say the value.
    Query,
}

/// How `:s` replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitute {
    /// Empty: the last pattern used.
    pub pattern: String,
    pub replacement: String,
    /// `g`: every match of a line, not only the first.
    pub global: bool,
    /// `i` / `I`: case forced off / on; `None` follows `ignorecase` and `smartcase`.
    pub ignore_case: Option<bool>,
    /// `n`: count, do not replace.
    pub count_only: bool,
    /// `e`: no error when nothing matches.
    pub quiet: bool,
}

/// What `:g` runs on each matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalCommand {
    Delete,
    Substitute(Substitute),
    Normal(String),
}

/// A parsed `:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ex {
    /// `:w`, `:w!`.
    Write {
        force: bool,
    },
    /// `:q`, `:q!`.
    Quit {
        force: bool,
    },
    /// `:wq` (always writes), `:x` (writes only a changed file).
    WriteQuit {
        only_if_changed: bool,
    },
    /// `:e`, `:e!`, `:e <path>`.
    Edit {
        path: Option<String>,
        force: bool,
    },
    /// `:sp`, `:vs`: the pane splits and the new half shows the same buffer.
    Split {
        vertical: bool,
    },
    /// `:b <number | part of a name>`.
    Buffer(String),
    /// `:ls`, `:buffers`.
    Buffers,
    /// `:term`: a terminal in the file's folder.
    Terminal,
    /// `:noh`: the find highlights go away.
    NoHighlight,
    Set(Vec<SetOption>),
    /// `:N`: to line N (a range alone).
    Goto(LineRef),
    Substitute {
        lines: Option<LineSpec>,
        command: Substitute,
    },
    Global {
        lines: Option<LineSpec>,
        invert: bool,
        pattern: String,
        command: GlobalCommand,
    },
    Normal {
        lines: Option<LineSpec>,
        keys: String,
    },
}

/// Why a line was not run: shown on the `:` line as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExError(pub String);

impl std::fmt::Display for ExError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn not_supported(what: &str) -> ExError {
    ExError(format!("Not supported: :{}", what.trim()))
}

/// Parses one `:` line (without the colon).
pub fn parse(line: &str) -> Result<Option<Ex>, ExError> {
    let line = line.trim_start_matches([' ', ':']).trim_end();
    if line.is_empty() || line.starts_with('"') {
        return Ok(None);
    }
    let mut rest = line;
    let lines = parse_lines(&mut rest)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return match lines {
            Some(LineSpec::One(target)) | Some(LineSpec::Two(_, target)) => {
                Ok(Some(Ex::Goto(target)))
            }
            Some(LineSpec::All) => Ok(Some(Ex::Goto(LineRef {
                address: Address::Last,
                offset: 0,
            }))),
            None => Ok(None),
        };
    }
    // The command name: letters, or one of the punctuation commands.
    let name_len = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphabetic())
        .map_or(rest.len(), |(at, _)| at);
    let (name, mut args) = rest.split_at(name_len);
    if name.is_empty() {
        return Err(not_supported(rest));
    }
    let force = args.starts_with('!');
    if force {
        args = &args[1..];
    }
    let arg = args.trim();
    let no_range = |ex: Ex| -> Result<Option<Ex>, ExError> {
        if lines.is_some() {
            Err(not_supported(line))
        } else {
            Ok(Some(ex))
        }
    };
    let no_args = |ex: Ex| -> Result<Option<Ex>, ExError> {
        if !arg.is_empty() || lines.is_some() {
            Err(not_supported(line))
        } else {
            Ok(Some(ex))
        }
    };
    match name {
        "w" | "write" => no_args(Ex::Write { force }),
        "q" | "quit" | "clo" | "close" => no_args(Ex::Quit { force }),
        "wq" => no_args(Ex::WriteQuit {
            only_if_changed: false,
        }),
        "x" | "xit" | "exi" | "exit" => no_args(Ex::WriteQuit {
            only_if_changed: true,
        }),
        "e" | "edit" => no_range(Ex::Edit {
            path: (!arg.is_empty()).then(|| arg.to_owned()),
            force,
        }),
        "sp" | "split" | "new" if !force => no_args(Ex::Split { vertical: false }),
        "vs" | "vsplit" | "vne" | "vnew" if !force => no_args(Ex::Split { vertical: true }),
        "b" | "buffer" if !force && !arg.is_empty() => no_range(Ex::Buffer(arg.to_owned())),
        "ls" | "buffers" | "files" if !force => no_args(Ex::Buffers),
        "ter" | "term" | "terminal" if !force => no_args(Ex::Terminal),
        "noh" | "nohlsearch" if !force => no_args(Ex::NoHighlight),
        "se" | "set" if !force => {
            if lines.is_some() || arg.is_empty() {
                return Err(not_supported(line));
            }
            arg.split_whitespace()
                .map(parse_set)
                .collect::<Result<Vec<_>, _>>()
                .map(|options| Some(Ex::Set(options)))
        }
        "s" | "substitute" if !force => Ok(Some(Ex::Substitute {
            lines,
            command: parse_substitute(args.trim_start(), line)?,
        })),
        "g" | "global" | "v" | "vglobal" => {
            let invert = force || name.starts_with('v');
            let (pattern, command) = split_pattern(args.trim_start(), line)?;
            let command = match command.trim() {
                "d" | "delete" => GlobalCommand::Delete,
                c if c.starts_with('s') => {
                    let body = c.trim_start_matches("substitute").trim_start_matches('s');
                    GlobalCommand::Substitute(parse_substitute(body, line)?)
                }
                c if c.starts_with("norm") => {
                    let keys = c
                        .trim_start_matches("normal")
                        .trim_start_matches("norm")
                        .trim_start_matches('!');
                    GlobalCommand::Normal(keys.strip_prefix(' ').unwrap_or(keys).to_owned())
                }
                "" => return Err(not_supported(line)),
                other => return Err(not_supported(&format!("g/…/{other}"))),
            };
            Ok(Some(Ex::Global {
                lines,
                invert,
                pattern,
                command,
            }))
        }
        "norm" | "normal" => {
            let keys = args.strip_prefix(' ').unwrap_or(args);
            if keys.is_empty() {
                return Err(ExError("E471: Argument required".into()));
            }
            Ok(Some(Ex::Normal {
                lines,
                keys: keys.to_owned(),
            }))
        }
        _ => Err(not_supported(line)),
    }
}

fn parse_set(word: &str) -> Result<SetOption, ExError> {
    let (word, query) = match word.strip_suffix('?') {
        Some(w) => (w, true),
        None => (word, false),
    };
    let (word, invert) = match word.strip_suffix('!') {
        Some(w) => (w, true),
        None => (word, false),
    };
    let (name, toggle) = if query {
        (word, Toggle::Query)
    } else if invert {
        (word, Toggle::Invert)
    } else if let Some(name) = word.strip_prefix("inv") {
        (name, Toggle::Invert)
    } else if let Some(name) = word.strip_prefix("no") {
        (name, Toggle::Off)
    } else {
        (word, Toggle::On)
    };
    match name {
        "nu" | "number" => Ok(SetOption::Number(toggle)),
        "ic" | "ignorecase" => Ok(SetOption::IgnoreCase(toggle)),
        "scs" | "smartcase" => Ok(SetOption::SmartCase(toggle)),
        _ => Err(ExError(format!("Not supported: :set {word}"))),
    }
}

/// `/pattern/rest`: the pattern (its delimiter escaped with `\`) and what follows it.
fn split_pattern<'a>(body: &'a str, line: &str) -> Result<(String, &'a str), ExError> {
    let mut chars = body.char_indices();
    let Some((_, delimiter)) = chars.next() else {
        return Err(not_supported(line));
    };
    if delimiter.is_alphanumeric() || delimiter == '\\' || delimiter == '"' || delimiter == ' ' {
        return Err(not_supported(line));
    }
    let mut pattern = String::new();
    let mut escaped = false;
    for (at, c) in chars {
        if escaped {
            if c != delimiter {
                pattern.push('\\');
            }
            pattern.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            return Ok((pattern, &body[at + c.len_utf8()..]));
        } else {
            pattern.push(c);
        }
    }
    if escaped {
        pattern.push('\\');
    }
    Ok((pattern, ""))
}

fn parse_substitute(body: &str, line: &str) -> Result<Substitute, ExError> {
    let delimiter = body.chars().next().ok_or_else(|| not_supported(line))?;
    let (pattern, rest) = split_pattern(body, line)?;
    // The replacement ends at the next unescaped delimiter; escapes stay for `expand`.
    let mut replacement = String::new();
    let mut flags = "";
    let mut escaped = false;
    let mut ended = false;
    for (at, c) in rest.char_indices() {
        if escaped {
            if c != delimiter {
                replacement.push('\\');
            }
            replacement.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            flags = &rest[at + c.len_utf8()..];
            ended = true;
            break;
        } else {
            replacement.push(c);
        }
    }
    if escaped {
        replacement.push('\\');
    }
    let mut command = Substitute {
        pattern,
        replacement,
        global: false,
        ignore_case: None,
        count_only: false,
        quiet: false,
    };
    if !ended {
        return Ok(command);
    }
    for flag in flags.trim().chars() {
        match flag {
            'g' => command.global = true,
            'i' => command.ignore_case = Some(true),
            'I' => command.ignore_case = Some(false),
            'n' => command.count_only = true,
            'e' => command.quiet = true,
            '&' => {}
            other => return Err(ExError(format!("Not supported: :s flag {other}"))),
        }
    }
    Ok(command)
}

/// Reads a range from the start of `rest` and moves past it.
fn parse_lines(rest: &mut &str) -> Result<Option<LineSpec>, ExError> {
    if let Some(after) = rest.strip_prefix('%') {
        *rest = after;
        return Ok(Some(LineSpec::All));
    }
    let Some(first) = parse_ref(rest)? else {
        return Ok(None);
    };
    if let Some(after) = rest.strip_prefix([',', ';']) {
        *rest = after;
        let second = parse_ref(rest)?.ok_or_else(|| ExError("E14: Invalid address".into()))?;
        return Ok(Some(LineSpec::Two(first, second)));
    }
    Ok(Some(LineSpec::One(first)))
}

fn parse_ref(rest: &mut &str) -> Result<Option<LineRef>, ExError> {
    let s = *rest;
    let (address, mut after) = if let Some(a) = s.strip_prefix('.') {
        (Some(Address::Current), a)
    } else if let Some(a) = s.strip_prefix('$') {
        (Some(Address::Last), a)
    } else if let Some(a) = s.strip_prefix("'<") {
        (Some(Address::VisualStart), a)
    } else if let Some(a) = s.strip_prefix("'>") {
        (Some(Address::VisualEnd), a)
    } else if s.starts_with(|c: char| c.is_ascii_digit()) {
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        let n = s[..digits]
            .parse::<usize>()
            .map_err(|_| ExError("E14: Invalid address".into()))?;
        (Some(Address::Line(n)), &s[digits..])
    } else if s.starts_with(['+', '-']) {
        (Some(Address::Current), s)
    } else {
        (None, s)
    };
    let Some(address) = address else {
        return Ok(None);
    };
    let mut offset = 0i64;
    while let Some(sign) = after.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits = after[1..].chars().take_while(char::is_ascii_digit).count();
        let n: i64 = if digits == 0 {
            1
        } else {
            after[1..1 + digits]
                .parse()
                .map_err(|_| ExError("E14: Invalid address".into()))?
        };
        offset += if sign == '+' { n } else { -n };
        after = &after[1 + digits..];
    }
    *rest = after;
    Ok(Some(LineRef { address, offset }))
}

/// Where a range is resolved: the caret's line and the last Visual selection's lines (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolve {
    pub current: usize,
    pub line_count: usize,
    pub visual: Option<(usize, usize)>,
}

impl Resolve {
    /// A 0-based line, or an error when it is outside the text.
    pub fn line(&self, at: LineRef) -> Result<usize, ExError> {
        let base = match at.address {
            Address::Line(0) => 0,
            Address::Line(n) => n as i64 - 1,
            Address::Current => self.current as i64,
            Address::Last => self.line_count as i64 - 1,
            Address::VisualStart => {
                self.visual
                    .ok_or_else(|| ExError("E20: Mark not set".into()))?
                    .0 as i64
            }
            Address::VisualEnd => {
                self.visual
                    .ok_or_else(|| ExError("E20: Mark not set".into()))?
                    .1 as i64
            }
        };
        let line = base + at.offset;
        if line < 0 || line >= self.line_count as i64 {
            return Err(ExError("E16: Invalid range".into()));
        }
        Ok(line as usize)
    }

    /// The 0-based lines of `spec` (`default` when there is none), first to last inclusive.
    pub fn lines(
        &self,
        spec: Option<LineSpec>,
        default: LineSpec,
    ) -> Result<Range<usize>, ExError> {
        match spec.unwrap_or(default) {
            LineSpec::All => Ok(0..self.line_count),
            LineSpec::One(at) => {
                let line = self.line(at)?;
                Ok(line..line + 1)
            }
            LineSpec::Two(a, b) => {
                let (a, b) = (self.line(a)?, self.line(b)?);
                if a > b {
                    return Err(ExError("E493: Backwards range given".into()));
                }
                Ok(a..b + 1)
            }
        }
    }
}

/// The current line alone (what `:s` and `:normal` act on without a range).
pub const CURRENT_LINE: LineSpec = LineSpec::One(LineRef {
    address: Address::Current,
    offset: 0,
});

/// The whole text (what `:g` acts on without a range).
pub const ALL_LINES: LineSpec = LineSpec::All;

/// Builds the regex of `pattern` (Vim's `\<` `\>` become word boundaries). `ignore_case` set
/// forces the case; else `ignorecase` with `smartcase` (an uppercase letter makes it sensitive).
pub fn compile(
    pattern: &str,
    ignore_case: Option<bool>,
    options: CaseOptions,
) -> Result<Regex, ExError> {
    let translated = pattern.replace("\\<", "\\b").replace("\\>", "\\b");
    let insensitive = ignore_case.unwrap_or_else(|| {
        options.ignore_case && !(options.smart_case && pattern.chars().any(char::is_uppercase))
    });
    RegexBuilder::new(&translated)
        .case_insensitive(insensitive)
        .size_limit(8 << 20)
        .build()
        .map_err(|e| ExError(format!("E486: Bad pattern: {}", first_line(&e.to_string()))))
}

fn first_line(s: &str) -> &str {
    s.lines().last().unwrap_or(s).trim()
}

/// `:set ignorecase` and `:set smartcase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaseOptions {
    pub ignore_case: bool,
    pub smart_case: bool,
}

/// The replacement text for one match: `&` / `\0` the match, `\1`–`\9` its groups, `\r` and
/// `\n` a line break, `\\` a backslash, `\&` an ampersand.
pub fn expand(replacement: &str, captures: &regex::Captures<'_>) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => out.push_str(captures.get(0).map_or("", |m| m.as_str())),
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => {
                    let group = d as usize - '0' as usize;
                    out.push_str(captures.get(group).map_or("", |m| m.as_str()));
                }
                Some('r' | 'n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    out
}

/// What `:s` found in `lines`: the edits (in text order) and how many matches on how many
/// lines; the line of the last substitution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Substitution {
    pub edits: Vec<Edit>,
    pub matches: usize,
    pub lines: usize,
    pub last_line: Option<usize>,
}

/// Runs `:s` over `lines` of `text` (`regex` compiled from the command). Pure: reads a
/// snapshot, returns edits.
pub fn substitute(
    text: &Text,
    lines: Range<usize>,
    regex: &Regex,
    command: &Substitute,
    cancelled: &dyn Fn() -> bool,
) -> Substitution {
    let mut out = Substitution::default();
    for line in lines {
        if line % 256 == 0 && cancelled() {
            return Substitution::default();
        }
        let Ok(content) = text.line_content_range(line) else {
            continue;
        };
        let Ok(line_text) = text.slice(content.clone()) else {
            continue;
        };
        let mut found = 0;
        for captures in regex.captures_iter(&line_text) {
            let Some(whole) = captures.get(0) else {
                continue;
            };
            found += 1;
            if !command.count_only {
                out.edits.push(Edit::new(
                    content.start + whole.start()..content.start + whole.end(),
                    expand(&command.replacement, &captures),
                ));
            }
            if !command.global {
                break;
            }
        }
        if found > 0 {
            out.matches += found;
            out.lines += 1;
            out.last_line = Some(line);
        }
    }
    out
}

/// The lines of `lines` whose text matches `regex` (or does not, `invert`).
pub fn matching_lines(
    text: &Text,
    lines: Range<usize>,
    regex: &Regex,
    invert: bool,
    cancelled: &dyn Fn() -> bool,
) -> Vec<usize> {
    let mut out = Vec::new();
    for line in lines {
        if line % 256 == 0 && cancelled() {
            return Vec::new();
        }
        let Ok(line_text) = text.line_text(line) else {
            continue;
        };
        if regex.is_match(&line_text) != invert {
            out.push(line);
        }
    }
    out
}

/// Edits that delete whole `lines` (sorted, 0-based) of `text`, adjacent lines merged.
pub fn delete_lines(text: &Text, lines: &[usize]) -> Vec<Edit> {
    let ends_with_break = text.char_before(text.len()) == Some('\n');
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &line in lines {
        let Ok(mut range) = text.line_range(line) else {
            continue;
        };
        // The last line has no break of its own: take the one before it.
        if range.end == text.len() && range.start > 0 && !ends_with_break {
            range.start -= 1;
        }
        match ranges.last_mut() {
            Some(last) if last.end >= range.start => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
    }
    ranges.into_iter().map(Edit::delete).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(line: &str) -> Ex {
        parse(line).unwrap().unwrap()
    }

    fn err(line: &str) -> String {
        parse(line).unwrap_err().0
    }

    #[test]
    fn files_windows_and_buffers() {
        assert_eq!(ex("w"), Ex::Write { force: false });
        assert_eq!(ex(":w!"), Ex::Write { force: true });
        assert_eq!(ex("q!"), Ex::Quit { force: true });
        assert_eq!(
            ex("wq"),
            Ex::WriteQuit {
                only_if_changed: false
            }
        );
        assert_eq!(
            ex("x"),
            Ex::WriteQuit {
                only_if_changed: true
            }
        );
        assert_eq!(
            ex("e src/lib.rs"),
            Ex::Edit {
                path: Some("src/lib.rs".into()),
                force: false
            }
        );
        assert_eq!(
            ex("e!"),
            Ex::Edit {
                path: None,
                force: true
            }
        );
        assert_eq!(ex("sp"), Ex::Split { vertical: false });
        assert_eq!(ex("vs"), Ex::Split { vertical: true });
        assert_eq!(ex("vsplit"), Ex::Split { vertical: true });
        assert_eq!(ex("b 2"), Ex::Buffer("2".into()));
        assert_eq!(ex("ls"), Ex::Buffers);
        assert_eq!(ex("term"), Ex::Terminal);
        assert_eq!(ex("noh"), Ex::NoHighlight);
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn unsupported_commands_say_so() {
        assert_eq!(err("foo"), "Not supported: :foo");
        assert_eq!(err("w other.txt"), "Not supported: :w other.txt");
        assert_eq!(err("sp other.txt"), "Not supported: :sp other.txt");
        assert_eq!(err("set wrap"), "Not supported: :set wrap");
        assert_eq!(err("s/a/b/c"), "Not supported: :s flag c");
        assert_eq!(err("g/x/p"), "Not supported: :g/…/p");
        assert_eq!(err("1,2w"), "Not supported: :1,2w");
    }

    #[test]
    fn ranges() {
        let resolve = Resolve {
            current: 4,
            line_count: 10,
            visual: Some((2, 3)),
        };
        let lines = |s: &str| {
            let Ex::Substitute { lines, .. } = ex(s) else {
                panic!("{s}");
            };
            resolve.lines(lines, CURRENT_LINE)
        };
        assert_eq!(lines("s/a/b/").unwrap(), 4..5);
        assert_eq!(lines("%s/a/b/").unwrap(), 0..10);
        assert_eq!(lines("2,$s/a/b/").unwrap(), 1..10);
        assert_eq!(lines(".,.+2s/a/b/").unwrap(), 4..7);
        assert_eq!(lines("'<,'>s/a/b/").unwrap(), 2..4);
        assert_eq!(lines("-1s/a/b/").unwrap(), 3..4);
        assert_eq!(
            lines("5,2s/a/b/").unwrap_err().0,
            "E493: Backwards range given"
        );
        assert_eq!(lines("20s/a/b/").unwrap_err().0, "E16: Invalid range");
        let Ex::Goto(at) = ex("12") else {
            panic!();
        };
        assert_eq!(at.address, Address::Line(12));
        let Ex::Goto(at) = ex("$") else {
            panic!();
        };
        assert_eq!(resolve.line(at).unwrap(), 9);
    }

    #[test]
    fn substitute_parses_delimiters_flags_and_escapes() {
        let Ex::Substitute { command, .. } = ex(r"s#a/b#c\#d#gi") else {
            panic!();
        };
        assert_eq!(command.pattern, "a/b");
        assert_eq!(command.replacement, "c#d");
        assert!(command.global);
        assert_eq!(command.ignore_case, Some(true));
        let Ex::Substitute { command, .. } = ex(r"s/\(x\)/\1\1") else {
            panic!();
        };
        assert_eq!(command.pattern, r"\(x\)");
        assert_eq!(command.replacement, r"\1\1");
        assert!(!command.global);
    }

    #[test]
    fn substitute_replaces_with_groups_and_counts() {
        let text = Text::from("let a = a;\nfoo\nlet b = a;\n");
        let command = Substitute {
            pattern: r"\ba\b".into(),
            replacement: "[&]".into(),
            global: true,
            ignore_case: None,
            count_only: false,
            quiet: false,
        };
        let regex = compile(&command.pattern, None, CaseOptions::default()).unwrap();
        let found = substitute(&text, 0..4, &regex, &command, &|| false);
        assert_eq!(found.matches, 3);
        assert_eq!(found.lines, 2);
        assert_eq!(found.last_line, Some(2));
        let mut s = text.to_string();
        for edit in found.edits.iter().rev() {
            s.replace_range(edit.range.clone(), &edit.text);
        }
        assert_eq!(s, "let [a] = [a];\nfoo\nlet b = [a];\n");

        let regex = compile("(o)(o)", None, CaseOptions::default()).unwrap();
        let caps = regex.captures("foo").unwrap();
        assert_eq!(expand(r"\2-\1\r\\&", &caps), "o-o\n\\oo");
    }

    #[test]
    fn smartcase_and_word_boundaries() {
        let options = CaseOptions {
            ignore_case: true,
            smart_case: true,
        };
        assert!(compile("abc", None, options).unwrap().is_match("ABC"));
        assert!(!compile("Abc", None, options).unwrap().is_match("ABC"));
        assert!(
            !compile("abc", Some(false), options)
                .unwrap()
                .is_match("ABC")
        );
        assert!(compile(r"\<is\>", None, options).unwrap().is_match("it is"));
        assert!(!compile(r"\<is\>", None, options).unwrap().is_match("this"));
    }

    #[test]
    fn global_finds_lines_and_deletes_them() {
        let text = Text::from("keep\ndrop 1\ndrop 2\nkeep\ndrop 3");
        let regex = compile("drop", None, CaseOptions::default()).unwrap();
        let lines = matching_lines(&text, 0..5, &regex, false, &|| false);
        assert_eq!(lines, vec![1, 2, 4]);
        let mut s = text.to_string();
        for edit in delete_lines(&text, &lines).iter().rev() {
            s.replace_range(edit.range.clone(), &edit.text);
        }
        assert_eq!(s, "keep\nkeep");
        assert_eq!(
            matching_lines(&text, 0..5, &regex, true, &|| false),
            vec![0, 3]
        );
        let Ex::Global {
            invert, command, ..
        } = ex("v/x/normal Ax")
        else {
            panic!();
        };
        assert!(invert);
        assert_eq!(command, GlobalCommand::Normal("Ax".into()));
        assert!(matches!(
            ex("g/x/s/a/b/g"),
            Ex::Global {
                command: GlobalCommand::Substitute(_),
                ..
            }
        ));
    }

    #[test]
    fn set_options() {
        assert_eq!(
            ex("set nonu ic scs!"),
            Ex::Set(vec![
                SetOption::Number(Toggle::Off),
                SetOption::IgnoreCase(Toggle::On),
                SetOption::SmartCase(Toggle::Invert),
            ])
        );
        assert_eq!(
            ex("set number?"),
            Ex::Set(vec![SetOption::Number(Toggle::Query)])
        );
    }
}
