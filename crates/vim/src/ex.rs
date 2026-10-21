//! Vim's `:` line: a small parser of the ex commands Hutan knows, with Vim's line ranges.
//!
//! Ranges: `%`, `.`, `$`, `N`, `'x` (any mark, `'<` `'>` included), `/pat/` and `?pat?`, `\/`
//! `\?` (the last search pattern), each with `+N` / `-N` offsets, joined by `,` or `;` (`;`
//! makes the first line current for the second). Commands: the text ones the modal layer runs
//! itself ([`Vim::ex`](crate::Vim::ex)): `:s :& :g :v :normal :d :y :pu :m :t :co :j :> :<
//! :k :mark :u :red :set :noh :N`, and the ones a host's owner runs (files, windows, buffers,
//! terminals). Anything else is answered with "Not supported: :…", never swallowed.

/// One end of a line range, before it is resolved against the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// `N`: a 1-based line number (`0` is "before the first line", for `:m` / `:t` / `:pu`).
    Line(usize),
    /// `.`: the caret's line.
    Current,
    /// `$`: the last line.
    Last,
    /// `'x`: a mark's line (`'<` and `'>` are the last Visual selection's).
    Mark(char),
    /// `/pat/` (forward) and `?pat?`: the next (previous) line that matches; an empty pattern
    /// is the last search pattern (`\/`, `\?`).
    Search { pattern: String, forward: bool },
}

/// An address and its `+N` / `-N` offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRef {
    pub address: Address,
    pub offset: i64,
}

impl LineRef {
    pub const CURRENT: LineRef = LineRef {
        address: Address::Current,
        offset: 0,
    };
}

/// The lines a command acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSpec {
    /// `%`: every line.
    All,
    One(LineRef),
    /// `a,b`, or `a;b` (`chained`: `b` is counted from `a`).
    Two {
        from: LineRef,
        to: LineRef,
        chained: bool,
    },
}

/// An option `:set` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionName {
    /// `number` (`nu`): the owner's (a gutter).
    Number,
    IgnoreCase,
    SmartCase,
    WrapScan,
    HlSearch,
    IncSearch,
    ExpandTab,
    ShiftRound,
    JoinSpaces,
    ShiftWidth,
    TabStop,
}

impl OptionName {
    pub fn name(self) -> &'static str {
        match self {
            OptionName::Number => "number",
            OptionName::IgnoreCase => "ignorecase",
            OptionName::SmartCase => "smartcase",
            OptionName::WrapScan => "wrapscan",
            OptionName::HlSearch => "hlsearch",
            OptionName::IncSearch => "incsearch",
            OptionName::ExpandTab => "expandtab",
            OptionName::ShiftRound => "shiftround",
            OptionName::JoinSpaces => "joinspaces",
            OptionName::ShiftWidth => "shiftwidth",
            OptionName::TabStop => "tabstop",
        }
    }

    /// Whether the option is a number (`sw=4`) rather than on/off.
    pub fn numeric(self) -> bool {
        matches!(self, OptionName::ShiftWidth | OptionName::TabStop)
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "nu" | "number" => OptionName::Number,
            "ic" | "ignorecase" => OptionName::IgnoreCase,
            "scs" | "smartcase" => OptionName::SmartCase,
            "ws" | "wrapscan" => OptionName::WrapScan,
            "hls" | "hlsearch" => OptionName::HlSearch,
            "is" | "incsearch" => OptionName::IncSearch,
            "et" | "expandtab" => OptionName::ExpandTab,
            "sr" | "shiftround" => OptionName::ShiftRound,
            "js" | "joinspaces" => OptionName::JoinSpaces,
            "sw" | "shiftwidth" => OptionName::ShiftWidth,
            "ts" | "tabstop" => OptionName::TabStop,
            _ => return None,
        })
    }
}

/// What `:set` does with an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    Invert,
    /// `name?` (or a number option's bare name): say the value.
    Query,
    /// `name=N`.
    Value(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetOption {
    pub name: OptionName,
    pub toggle: Toggle,
}

/// How `:s` replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitute {
    /// Empty: the last search pattern (`:s//x/`).
    pub pattern: String,
    /// `None`: `:s` / `:&` without a pattern, the last substitute again.
    pub replacement: Option<String>,
    /// `g`: every match of a line, not only the first.
    pub global: bool,
    /// `i` / `I`: case forced off / on; `None` follows `ignorecase` and `smartcase`.
    pub ignore_case: Option<bool>,
    /// `n`: count, do not replace.
    pub count_only: bool,
    /// `e`: no error when nothing matches.
    pub quiet: bool,
    /// `&` (first flag), `:&&`: the flags of the last substitute too.
    pub keep_flags: bool,
    /// `:s/a/b/ 3`: three lines from the last line of the range.
    pub count: Option<usize>,
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
    /// `:noh`: the search highlights go away.
    NoHighlight,
    Set(Vec<SetOption>),
    /// `:N`: to line N (a range alone).
    Goto(LineRef),
    Substitute {
        lines: Option<LineSpec>,
        command: Substitute,
    },
    /// `:g/pat/cmd`, `:g!` / `:v` (`invert`): `command` on every matching line.
    Global {
        lines: Option<LineSpec>,
        invert: bool,
        pattern: String,
        command: Box<Ex>,
    },
    Normal {
        lines: Option<LineSpec>,
        keys: String,
    },
    /// `:d [x] [count]`.
    Delete {
        lines: Option<LineSpec>,
        register: Option<char>,
        count: Option<usize>,
    },
    /// `:y [x] [count]`.
    Yank {
        lines: Option<LineSpec>,
        register: Option<char>,
        count: Option<usize>,
    },
    /// `:pu [x]` below the line, `:pu! [x]` above it.
    Put {
        line: Option<LineRef>,
        register: Option<char>,
        above: bool,
    },
    /// `:m {address}`.
    Move {
        lines: Option<LineSpec>,
        to: LineRef,
    },
    /// `:t {address}`, `:co {address}`.
    Copy {
        lines: Option<LineSpec>,
        to: LineRef,
    },
    /// `:j [count]`; `:j!` keeps the lines' white space.
    Join {
        lines: Option<LineSpec>,
        count: Option<usize>,
        keep_spaces: bool,
    },
    /// `:>` / `:<` (`amount` times `'shiftwidth'`, one per `>`), `[count]` lines.
    Shift {
        lines: Option<LineSpec>,
        left: bool,
        amount: usize,
        count: Option<usize>,
    },
    /// `:k x`, `:mark x`.
    Mark {
        line: Option<LineRef>,
        name: char,
    },
    /// `:u`.
    Undo,
    /// `:red`.
    Redo,
}

impl Ex {
    /// Whether the host's owner runs the command (files, windows, buffers, terminals, the
    /// search highlights) rather than the modal layer.
    pub fn is_owners(&self) -> bool {
        matches!(
            self,
            Ex::Write { .. }
                | Ex::Quit { .. }
                | Ex::WriteQuit { .. }
                | Ex::Edit { .. }
                | Ex::Split { .. }
                | Ex::Buffer(_)
                | Ex::Buffers
                | Ex::Terminal
                | Ex::NoHighlight
        )
    }
}

/// Why a line was not run: shown on the `:` line as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExError(pub String);

impl std::fmt::Display for ExError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExError {}

pub(crate) fn not_supported(what: &str) -> ExError {
    ExError(format!("Not supported: :{}", what.trim()))
}

/// Parses one `:` line (without the colon); `None` for an empty line or a comment.
pub fn parse(line: &str) -> Result<Option<Ex>, ExError> {
    // Trailing blanks matter (`:normal A `), so only the start is trimmed.
    let line = line.trim_start_matches([' ', ':']);
    if line.trim().is_empty() || line.starts_with('"') {
        return Ok(None);
    }
    parse_command(line, false).map(Some)
}

fn parse_command(line: &str, in_global: bool) -> Result<Ex, ExError> {
    let mut rest = line;
    let lines = parse_lines(&mut rest)?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return match lines {
            Some(LineSpec::One(target)) | Some(LineSpec::Two { to: target, .. }) => {
                Ok(Ex::Goto(target))
            }
            Some(LineSpec::All) => Ok(Ex::Goto(LineRef {
                address: Address::Last,
                offset: 0,
            })),
            None => Err(not_supported(line)),
        };
    }
    // The punctuation commands.
    if let Some(after) = rest.strip_prefix(['>', '<']) {
        let left = rest.starts_with('<');
        let more = after
            .chars()
            .take_while(|c| *c == rest.as_bytes()[0] as char)
            .count();
        let arg = after[more..].trim();
        return Ok(Ex::Shift {
            lines,
            left,
            amount: 1 + more,
            count: parse_count(arg, line)?,
        });
    }
    if let Some(after) = rest.strip_prefix('&') {
        let (keep, flags) = match after.strip_prefix('&') {
            Some(f) => (true, f),
            None => (false, after),
        };
        let mut command = Substitute {
            pattern: String::new(),
            replacement: None,
            global: false,
            ignore_case: None,
            count_only: false,
            quiet: false,
            keep_flags: keep,
            count: None,
        };
        parse_flags(flags, &mut command, line)?;
        return Ok(Ex::Substitute { lines, command });
    }
    // The command name: letters.
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
    let no_range = |ex: Ex| -> Result<Ex, ExError> {
        if lines.is_some() {
            Err(not_supported(line))
        } else {
            Ok(ex)
        }
    };
    let no_args = |ex: Ex| -> Result<Ex, ExError> {
        if !arg.is_empty() || lines.is_some() {
            Err(not_supported(line))
        } else {
            Ok(ex)
        }
    };
    let one_line = |lines: Option<LineSpec>| -> Result<Option<LineRef>, ExError> {
        match lines {
            None => Ok(None),
            Some(LineSpec::One(at)) | Some(LineSpec::Two { to: at, .. }) => Ok(Some(at)),
            Some(LineSpec::All) => Ok(Some(LineRef {
                address: Address::Last,
                offset: 0,
            })),
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
        "u" | "un" | "undo" if !force => no_args(Ex::Undo),
        "red" | "redo" if !force => no_args(Ex::Redo),
        "se" | "set" if !force => {
            if lines.is_some() || arg.is_empty() {
                return Err(not_supported(line));
            }
            arg.split_whitespace()
                .map(parse_set)
                .collect::<Result<Vec<_>, _>>()
                .map(Ex::Set)
        }
        "s" | "su" | "substitute" if !force => Ok(Ex::Substitute {
            lines,
            command: parse_substitute(args, line)?,
        }),
        "g" | "gl" | "global" | "v" | "vg" | "vglobal" => {
            if in_global {
                return Err(ExError("E147: Cannot do :global recursive".into()));
            }
            let invert = force || name.starts_with('v');
            let (pattern, command) = split_pattern(args.trim_start(), line)?;
            let command = command.trim_start();
            if command.is_empty() {
                return Err(not_supported(&format!("{name}/…/p")));
            }
            let command = parse_command(command, true).map_err(|e| {
                if e.0.starts_with("Not supported") {
                    not_supported(&format!("{name}/…/{command}"))
                } else {
                    e
                }
            })?;
            Ok(Ex::Global {
                lines,
                invert,
                pattern,
                command: Box::new(command),
            })
        }
        "norm" | "norma" | "normal" => {
            let keys = args.strip_prefix(' ').unwrap_or(args);
            if keys.is_empty() {
                return Err(ExError("E471: Argument required".into()));
            }
            Ok(Ex::Normal {
                lines,
                keys: keys.to_owned(),
            })
        }
        "d" | "de" | "del" | "dele" | "delet" | "delete" if !force => {
            let (register, count) = parse_register_count(arg, line)?;
            Ok(Ex::Delete {
                lines,
                register,
                count,
            })
        }
        "y" | "ya" | "yan" | "yank" if !force => {
            let (register, count) = parse_register_count(arg, line)?;
            Ok(Ex::Yank {
                lines,
                register,
                count,
            })
        }
        "pu" | "put" => {
            let register = match arg.chars().next() {
                None => None,
                Some(c) if crate::register::valid_name(c) && arg.chars().count() == 1 => Some(c),
                Some(_) => return Err(ExError("E488: Trailing characters".into())),
            };
            Ok(Ex::Put {
                line: one_line(lines)?,
                register,
                above: force,
            })
        }
        "m" | "mo" | "mov" | "move" if !force => Ok(Ex::Move {
            lines,
            to: parse_destination(arg)?,
        }),
        "t" | "co" | "cop" | "copy" if !force => Ok(Ex::Copy {
            lines,
            to: parse_destination(arg)?,
        }),
        "j" | "jo" | "joi" | "join" => Ok(Ex::Join {
            lines,
            count: parse_count(arg, line)?,
            keep_spaces: force,
        }),
        "k" | "ma" | "mar" | "mark" if !force => {
            let mut chars = arg.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None)
                    if c.is_ascii_alphabetic()
                        || matches!(c, '\'' | '`' | '<' | '>' | '[' | ']') =>
                {
                    Ok(Ex::Mark {
                        line: one_line(lines)?,
                        name: c,
                    })
                }
                (None, _) => Err(ExError("E471: Argument required".into())),
                _ => Err(ExError(
                    "E191: Argument must be a letter or forward/backward quote".into(),
                )),
            }
        }
        // `:ka`: `:k` and the mark without a space.
        k if k.len() == 2 && k.starts_with('k') && !force && arg.is_empty() => Ok(Ex::Mark {
            line: one_line(lines)?,
            name: k.chars().nth(1).unwrap_or('a'),
        }),
        _ => Err(not_supported(line)),
    }
}

fn parse_count(arg: &str, line: &str) -> Result<Option<usize>, ExError> {
    if arg.is_empty() {
        return Ok(None);
    }
    match arg.parse::<usize>() {
        Ok(0) => Err(ExError("E939: Positive count required".into())),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(not_supported(line)),
    }
}

/// `[x] [count]` of `:d` and `:y`.
fn parse_register_count(arg: &str, line: &str) -> Result<(Option<char>, Option<usize>), ExError> {
    let mut rest = arg;
    let mut register = None;
    if let Some(c) = rest.chars().next()
        && !c.is_ascii_digit()
    {
        if !crate::register::valid_name(c) || matches!(c, '.' | ':' | '/') {
            return Err(ExError("E488: Trailing characters".into()));
        }
        register = Some(c);
        rest = rest[c.len_utf8()..].trim_start();
    }
    Ok((register, parse_count(rest, line)?))
}

fn parse_destination(arg: &str) -> Result<LineRef, ExError> {
    let mut rest = arg;
    let to = parse_ref(&mut rest)?.ok_or_else(|| ExError("E14: Invalid address".into()))?;
    if !rest.trim().is_empty() {
        return Err(ExError("E488: Trailing characters".into()));
    }
    Ok(to)
}

fn parse_set(word: &str) -> Result<SetOption, ExError> {
    if let Some((name, value)) = word.split_once(['=', ':']) {
        let option = OptionName::parse(name)
            .filter(|o| o.numeric())
            .ok_or_else(|| ExError(format!("Not supported: :set {word}")))?;
        let value = value
            .parse::<usize>()
            .map_err(|_| ExError(format!("E521: Number required after =: {word}")))?;
        if option == OptionName::TabStop && value == 0 {
            return Err(ExError(format!("E487: Argument must be positive: {word}")));
        }
        return Ok(SetOption {
            name: option,
            toggle: Toggle::Value(value),
        });
    }
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
    } else if let Some(name) = word
        .strip_prefix("inv")
        .filter(|n| OptionName::parse(n).is_some())
    {
        (name, Toggle::Invert)
    } else if let Some(name) = word
        .strip_prefix("no")
        .filter(|n| OptionName::parse(n).is_some())
    {
        (name, Toggle::Off)
    } else {
        (word, Toggle::On)
    };
    let option =
        OptionName::parse(name).ok_or_else(|| ExError(format!("Not supported: :set {word}")))?;
    let toggle = match (option.numeric(), toggle) {
        (true, Toggle::On) => Toggle::Query,
        (true, Toggle::Off | Toggle::Invert) => {
            return Err(ExError(format!("E474: Invalid argument: {word}")));
        }
        (_, t) => t,
    };
    Ok(SetOption {
        name: option,
        toggle,
    })
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
    Ok(take_until(&body[delimiter.len_utf8()..], delimiter))
}

/// The text up to the next unescaped `delimiter` (`\delimiter` becomes the delimiter, other
/// escapes stay) and what follows the delimiter.
fn take_until(body: &str, delimiter: char) -> (String, &str) {
    let mut out = String::new();
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        if escaped {
            if c != delimiter {
                out.push('\\');
            }
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delimiter {
            return (out, &body[at + c.len_utf8()..]);
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    (out, "")
}

fn parse_substitute(args: &str, line: &str) -> Result<Substitute, ExError> {
    let mut command = Substitute {
        pattern: String::new(),
        replacement: None,
        global: false,
        ignore_case: None,
        count_only: false,
        quiet: false,
        keep_flags: false,
        count: None,
    };
    let body = args.trim_start();
    // `:s` alone or with flags (`:s g`, `:s&`): the last substitute again.
    match body.chars().next() {
        None => return Ok(command),
        Some(c) if c == '&' || c.is_ascii_alphanumeric() => {
            parse_flags(body, &mut command, line)?;
            return Ok(command);
        }
        _ => {}
    }
    let delimiter = body.chars().next().ok_or_else(|| not_supported(line))?;
    let (pattern, rest) = split_pattern(body, line)?;
    command.pattern = pattern;
    // The replacement ends at the next unescaped delimiter; escapes stay for `expand`.
    let mut replacement = String::new();
    let mut flags = "";
    let mut escaped = false;
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
            break;
        } else {
            replacement.push(c);
        }
    }
    if escaped {
        replacement.push('\\');
    }
    command.replacement = Some(replacement);
    parse_flags(flags, &mut command, line)?;
    Ok(command)
}

/// `[&][flags] [count]` after `:s/a/b/`, `:&` and `:s`.
fn parse_flags(flags: &str, command: &mut Substitute, line: &str) -> Result<(), ExError> {
    let flags = flags.trim_start();
    let mut rest = flags;
    if let Some(after) = rest.strip_prefix('&') {
        command.keep_flags = true;
        rest = after;
    }
    let letters = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    for flag in rest[..letters].chars() {
        match flag {
            'g' => command.global = !command.global,
            'i' => command.ignore_case = Some(true),
            'I' => command.ignore_case = Some(false),
            'n' => command.count_only = true,
            'e' => command.quiet = true,
            other => return Err(ExError(format!("Not supported: :s flag {other}"))),
        }
    }
    command.count = parse_count(rest[letters..].trim(), line)?;
    Ok(())
}

/// Reads a range from the start of `rest` and moves past it.
fn parse_lines(rest: &mut &str) -> Result<Option<LineSpec>, ExError> {
    *rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix('%') {
        *rest = after;
        return Ok(Some(LineSpec::All));
    }
    let first = parse_ref(rest)?;
    let Some(sep) = rest.chars().next().filter(|c| *c == ',' || *c == ';') else {
        return Ok(first.map(LineSpec::One));
    };
    *rest = &rest[1..];
    let from = first.unwrap_or(LineRef::CURRENT);
    // `a,` alone is `a,.`.
    let to = parse_ref(rest)?.unwrap_or(LineRef::CURRENT);
    Ok(Some(LineSpec::Two {
        from,
        to,
        chained: sep == ';',
    }))
}

fn parse_ref(rest: &mut &str) -> Result<Option<LineRef>, ExError> {
    let s = rest.trim_start();
    let (address, mut after) = if let Some(a) = s.strip_prefix('.') {
        (Some(Address::Current), a)
    } else if let Some(a) = s.strip_prefix('$') {
        (Some(Address::Last), a)
    } else if let Some(a) = s.strip_prefix('\'') {
        let mark = a
            .chars()
            .next()
            .ok_or_else(|| ExError("E20: Mark not set".into()))?;
        if !crate::marks::readable(mark) {
            return Err(ExError("E20: Mark not set".into()));
        }
        (Some(Address::Mark(mark)), &a[mark.len_utf8()..])
    } else if let Some(a) = s.strip_prefix(['/', '?']) {
        let forward = s.starts_with('/');
        let (pattern, after) = take_until(a, if forward { '/' } else { '?' });
        (Some(Address::Search { pattern, forward }), after)
    } else if let Some(a) = s.strip_prefix("\\/") {
        (
            Some(Address::Search {
                pattern: String::new(),
                forward: true,
            }),
            a,
        )
    } else if let Some(a) = s.strip_prefix("\\?") {
        (
            Some(Address::Search {
                pattern: String::new(),
                forward: false,
            }),
            a,
        )
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
    loop {
        let trimmed = after.trim_start();
        let Some(sign) = trimmed.chars().next().filter(|c| *c == '+' || *c == '-') else {
            break;
        };
        let body = &trimmed[1..];
        let digits = body.chars().take_while(char::is_ascii_digit).count();
        let n: i64 = if digits == 0 {
            1
        } else {
            body[..digits]
                .parse()
                .map_err(|_| ExError("E14: Invalid address".into()))?
        };
        offset += if sign == '+' { n } else { -n };
        after = &body[digits..];
    }
    *rest = after;
    Ok(Some(LineRef { address, offset }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

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
        assert_eq!(ex("sp"), Ex::Split { vertical: false });
        assert_eq!(ex("vsplit"), Ex::Split { vertical: true });
        assert_eq!(ex("b 2"), Ex::Buffer("2".into()));
        assert_eq!(ex("ls"), Ex::Buffers);
        assert_eq!(ex("term"), Ex::Terminal);
        assert_eq!(ex("noh"), Ex::NoHighlight);
        assert_eq!(parse("").unwrap(), None);
        assert!(ex("w").is_owners());
        assert!(!ex("d").is_owners());
    }

    #[test]
    fn unsupported_commands_say_so() {
        assert_eq!(err("foo"), "Not supported: :foo");
        assert_eq!(err("w other.txt"), "Not supported: :w other.txt");
        assert_eq!(err("sp other.txt"), "Not supported: :sp other.txt");
        assert_eq!(err("set wrap"), "Not supported: :set wrap");
        assert_eq!(err("s/a/b/c"), "Not supported: :s flag c");
        assert_eq!(err("g/x/p"), "Not supported: :g/…/p");
        assert_eq!(err("g/x"), "Not supported: :g/…/p");
        assert_eq!(err("1,2w"), "Not supported: :1,2w");
        assert_eq!(err("!ls"), "Not supported: :!ls");
        assert_eq!(err("g/a/g/b/d"), "E147: Cannot do :global recursive");
    }

    #[test]
    fn ranges_parse() {
        let Ex::Delete { lines, .. } = ex("'a,/foo/+1d") else {
            panic!()
        };
        assert_eq!(
            lines,
            Some(LineSpec::Two {
                from: LineRef {
                    address: Address::Mark('a'),
                    offset: 0
                },
                to: LineRef {
                    address: Address::Search {
                        pattern: "foo".into(),
                        forward: true
                    },
                    offset: 1
                },
                chained: false,
            })
        );
        let Ex::Goto(at) = ex("12") else { panic!() };
        assert_eq!(at.address, Address::Line(12));
        let Ex::Substitute { lines, .. } = ex(".;+2s/a/b/") else {
            panic!()
        };
        assert!(matches!(lines, Some(LineSpec::Two { chained: true, .. })));
    }

    #[test]
    fn substitute_parses_delimiters_flags_and_escapes() {
        let Ex::Substitute { command, .. } = ex(r"s#a/b#c\#d#gi 3") else {
            panic!();
        };
        assert_eq!(command.pattern, "a/b");
        assert_eq!(command.replacement.as_deref(), Some("c#d"));
        assert!(command.global);
        assert_eq!(command.ignore_case, Some(true));
        assert_eq!(command.count, Some(3));
        let Ex::Substitute { command, .. } = ex(r"s/\(x\)/\1\1") else {
            panic!();
        };
        assert_eq!(command.pattern, r"\(x\)");
        assert_eq!(command.replacement.as_deref(), Some(r"\1\1"));
        let Ex::Substitute { command, .. } = ex("&&") else {
            panic!()
        };
        assert!(command.keep_flags);
        assert_eq!(command.replacement, None);
    }

    #[test]
    fn line_commands() {
        assert_eq!(
            ex("2,3d a 4"),
            Ex::Delete {
                lines: Some(LineSpec::Two {
                    from: LineRef {
                        address: Address::Line(2),
                        offset: 0
                    },
                    to: LineRef {
                        address: Address::Line(3),
                        offset: 0
                    },
                    chained: false
                }),
                register: Some('a'),
                count: Some(4),
            }
        );
        assert_eq!(
            ex("m0"),
            Ex::Move {
                lines: None,
                to: LineRef {
                    address: Address::Line(0),
                    offset: 0
                }
            }
        );
        assert!(matches!(
            ex(">> 2"),
            Ex::Shift {
                left: false,
                amount: 2,
                count: Some(2),
                ..
            }
        ));
        assert!(matches!(ex("ka"), Ex::Mark { name: 'a', .. }));
        assert!(matches!(
            ex("pu! b"),
            Ex::Put {
                above: true,
                register: Some('b'),
                ..
            }
        ));
        let Ex::Global {
            command, invert, ..
        } = ex("v/x/normal Ax")
        else {
            panic!();
        };
        assert!(invert);
        assert_eq!(
            *command,
            Ex::Normal {
                lines: None,
                keys: "Ax".into()
            }
        );
    }

    #[test]
    fn set_options() {
        assert_eq!(
            ex("set nonu ic scs! sw=4 ts?"),
            Ex::Set(vec![
                SetOption {
                    name: OptionName::Number,
                    toggle: Toggle::Off
                },
                SetOption {
                    name: OptionName::IgnoreCase,
                    toggle: Toggle::On
                },
                SetOption {
                    name: OptionName::SmartCase,
                    toggle: Toggle::Invert
                },
                SetOption {
                    name: OptionName::ShiftWidth,
                    toggle: Toggle::Value(4)
                },
                SetOption {
                    name: OptionName::TabStop,
                    toggle: Toggle::Query
                },
            ])
        );
    }
}
