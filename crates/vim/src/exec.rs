//! Runs the `:` commands that concern the text ([`Ex`]) on a host: ranges, `:s`, `:g`,
//! `:normal`, line commands, `:set`.

use std::ops::Range;

use crate::ex::{
    Address, Ex, ExError, LineRef, LineSpec, OptionName, SetOption, Substitute, Toggle,
    not_supported,
};
use crate::host::{Clipboard, Edit, Transaction, VimHost};
use crate::key::{Mode, Options};
use crate::marks::Pos;
use crate::pattern::{Pattern, with_previous};
use crate::register::{Register, Write};
use crate::text::Text;
use crate::vim::{Core, LastSearch, LastSubstitute, text_keys};

const CURRENT: LineSpec = LineSpec::One(LineRef::CURRENT);

fn owner_name(command: &Ex) -> &'static str {
    match command {
        Ex::Write { .. } => "w",
        Ex::Quit { .. } => "q",
        Ex::WriteQuit {
            only_if_changed: false,
        } => "wq",
        Ex::WriteQuit { .. } => "x",
        Ex::Edit { .. } => "e",
        Ex::Split { vertical: false } => "sp",
        Ex::Split { .. } => "vs",
        Ex::Buffer(_) => "b",
        Ex::Buffers => "ls",
        Ex::Terminal => "term",
        _ => "",
    }
}

impl Core {
    // ---- ranges --------------------------------------------------------------------------------

    /// A line of an address, 0-based; `-1` is "before the first line" (`:m 0`).
    fn address<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        at: &LineRef,
        current: usize,
    ) -> Result<isize, ExError> {
        let buf = Text::new(host);
        let count = buf.line_count() as isize;
        let base: isize = match &at.address {
            Address::Line(n) => *n as isize - 1,
            Address::Current => current as isize,
            Address::Last => count - 1,
            Address::Mark(name) => match self.marks.get(*name) {
                Some(pos) => pos.line as isize,
                None => return Err(ExError("E20: Mark not set".into())),
            },
            Address::Search { pattern, forward } => {
                let pattern = if pattern.is_empty() {
                    self.last_search
                        .as_ref()
                        .map(|s| s.pattern.clone())
                        .ok_or_else(|| ExError("E35: No previous regular expression".into()))?
                } else {
                    pattern.clone()
                };
                self.last_search = Some(LastSearch {
                    pattern: pattern.clone(),
                    forward: *forward,
                    offset: String::new(),
                });
                self.regs.last_search = Some(pattern.clone());
                let compiled = Pattern::new(
                    &pattern,
                    None,
                    self.settings.case(),
                    self.last_replacement.as_deref(),
                )?;
                let n = buf.line_count();
                let mut found = None;
                for step in 1..=n {
                    let line = if *forward {
                        (current + step) % n
                    } else {
                        (current + n - step % n) % n
                    };
                    if !self.settings.wrap_scan
                        && ((*forward && line <= current) || (!*forward && line >= current))
                    {
                        break;
                    }
                    if compiled.is_match(&buf.line_text(line)) {
                        found = Some(line);
                        break;
                    }
                }
                found.ok_or_else(|| ExError(format!("E486: Pattern not found: {pattern}")))?
                    as isize
            }
        };
        let line = base + at.offset as isize;
        if line < -1 || line >= count {
            return Err(ExError("E16: Invalid range".into()));
        }
        Ok(line)
    }

    fn line<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        at: &LineRef,
        current: usize,
    ) -> Result<usize, ExError> {
        let line = self.address(host, at, current)?;
        if line < 0 {
            // Line 0 is the first line for a range (`:0,3d` is `:1,3d`).
            return Ok(0);
        }
        Ok(line as usize)
    }

    /// The 0-based lines of `spec` (`default` without one), first to last inclusive.
    pub fn lines<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        spec: Option<&LineSpec>,
        default: &LineSpec,
    ) -> Result<Range<usize>, ExError> {
        let buf = Text::new(host);
        let current = buf.line_of(host.cursor());
        match spec.unwrap_or(default) {
            LineSpec::All => Ok(0..buf.line_count()),
            LineSpec::One(at) => {
                let line = self.line(host, at, current)?;
                Ok(line..line + 1)
            }
            LineSpec::Two { from, to, chained } => {
                let a = self.line(host, from, current)?;
                let b = self.line(host, to, if *chained { a } else { current })?;
                if a > b {
                    return Err(ExError("E493: Backwards range given".into()));
                }
                Ok(a..b + 1)
            }
        }
    }

    /// The lines with a `[count]`: `count` lines from the last line of the range.
    fn counted<H: VimHost + ?Sized>(
        &mut self,
        host: &H,
        spec: Option<&LineSpec>,
        count: Option<usize>,
    ) -> Result<Range<usize>, ExError> {
        let range = self.lines(host, spec, &CURRENT)?;
        Ok(match count {
            Some(n) => {
                let last = Text::new(host).line_count();
                let start = range.end - 1;
                start..(start + n).min(last)
            }
            None => range,
        })
    }

    // ---- commands ------------------------------------------------------------------------------

    pub fn run_ex<H: VimHost>(
        &mut self,
        host: &mut H,
        command: Ex,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Result<(), ExError> {
        if command.is_owners() {
            return match command {
                Ex::NoHighlight => Ok(()),
                other => Err(not_supported(owner_name(&other))),
            };
        }
        match command {
            Ex::Set(options) => self.set_options(&options),
            Ex::Goto(at) => {
                let current = Text::new(&*host).line_of(host.cursor());
                let line = self.address(host, &at, current)?.max(0) as usize;
                let from = host.cursor();
                self.marks.jump_from(Self::pos(host, from));
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(line));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Substitute { lines, command } => self.substitute(host, lines.as_ref(), command),
            Ex::Global {
                lines,
                invert,
                pattern,
                command,
            } => self.global(host, lines.as_ref(), invert, &pattern, *command, opts, clip),
            Ex::Normal { lines, keys } => {
                let lines: Option<Vec<usize>> = match lines {
                    Some(spec) => Some(self.lines(host, Some(&spec), &CURRENT)?.collect()),
                    None => None,
                };
                self.normal_on_lines(host, lines.as_deref(), &keys, opts, clip);
                Ok(())
            }
            Ex::Delete {
                lines,
                register,
                count,
            } => {
                let range = self.counted(host, lines.as_ref(), count)?;
                let buf = Text::new(&*host);
                let start = buf.line_start_of(range.start);
                let end = buf.line_end(buf.line_start_of(range.end - 1));
                let text = host.slice(start..end).into_owned();
                let write = Write::Delete {
                    one: true,
                    small: false,
                };
                self.ex_store(register, Register::lines(text), write, opts, clip);
                host.begin_group();
                let buf = Text::new(&*host);
                let (s, e) = if end < buf.len() {
                    (start, end + 1)
                } else if start > 0 {
                    (start - 1, end)
                } else {
                    (start, end)
                };
                self.apply(host, Transaction::delete(s..e, s));
                let after = Text::new(&*host);
                let line = range.start.min(after.line_count() - 1);
                let to = after.first_nonblank(after.line_start_of(line));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Yank {
                lines,
                register,
                count,
            } => {
                let range = self.counted(host, lines.as_ref(), count)?;
                let buf = Text::new(&*host);
                let start = buf.line_start_of(range.start);
                let end = buf.line_end(buf.line_start_of(range.end - 1));
                let text = host.slice(start..end).into_owned();
                self.ex_store(register, Register::lines(text), Write::Yank, opts, clip);
                Ok(())
            }
            Ex::Put {
                line,
                register,
                above,
            } => {
                let buf = Text::new(&*host);
                let current = buf.line_of(host.cursor());
                let target = match &line {
                    Some(at) => self.address(host, at, current)?,
                    None => current as isize,
                };
                let reg = match register {
                    Some('+' | '*') => clip.read().map(Register::from_clipboard),
                    Some(name) => self.regs.get(name),
                    None => self.regs.get('"'),
                }
                .ok_or_else(|| {
                    ExError(format!(
                        "E353: Nothing in register {}",
                        register.unwrap_or('"')
                    ))
                })?;
                let text = reg.text;
                let lines = text.matches('\n').count() + 1;
                // `:pu` puts below the line, `:pu!` above it; line 0 is above the first.
                let (after, first_new) = if above || target < 0 {
                    let line = target.max(0) as usize;
                    (None, line)
                } else {
                    (Some(target as usize), target as usize + 1)
                };
                host.begin_group();
                let buf = Text::new(&*host);
                let tx = match after {
                    Some(line) => {
                        let le = buf.line_end(buf.line_start_of(line));
                        Transaction::insert(le, format!("\n{text}"), le + 1)
                    }
                    None => {
                        let ls = buf.line_start_of(first_new);
                        Transaction::insert(ls, format!("{text}\n"), ls)
                    }
                };
                self.apply(host, tx);
                let after = Text::new(&*host);
                let last = first_new + lines - 1;
                let to = after.first_nonblank(after.line_start_of(last));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Move { lines, to } => {
                let range = self.lines(host, lines.as_ref(), &CURRENT)?;
                let buf = Text::new(&*host);
                let current = buf.line_of(host.cursor());
                let dest = self.address(host, &to, current)?;
                let (a, b) = (range.start as isize, range.end as isize - 1);
                if dest >= a && dest < b {
                    return Err(ExError(
                        "E134: Cannot move a range of lines into itself".into(),
                    ));
                }
                let n = (b - a + 1) as usize;
                if dest == b || dest == a - 1 {
                    // Already there.
                    let buf = Text::new(&*host);
                    let last = if dest < a { b } else { dest };
                    let to = buf.first_nonblank(buf.line_start_of(last as usize));
                    host.set_cursor(to);
                    return Ok(());
                }
                let moved: Vec<String> = range
                    .clone()
                    .map(|l| buf.line_text(l).into_owned())
                    .collect();
                let (lo, hi) = if dest < a {
                    ((dest + 1) as usize, b as usize)
                } else {
                    (a as usize, dest as usize)
                };
                let all: Vec<String> = (lo..=hi).map(|l| buf.line_text(l).into_owned()).collect();
                let new: Vec<String> = if dest < a {
                    let before = &all[..all.len() - n];
                    moved.iter().chain(before.iter()).cloned().collect()
                } else {
                    let after = &all[n..];
                    after.iter().chain(moved.iter()).cloned().collect()
                };
                let start = buf.line_start_of(lo);
                let end = buf.line_end(buf.line_start_of(hi));
                host.begin_group();
                self.apply(
                    host,
                    Transaction::replace(start..end, new.join("\n"), start),
                );
                let last = if dest < a {
                    (dest + n as isize) as usize
                } else {
                    dest as usize
                };
                let after = Text::new(&*host);
                let to = after.first_nonblank(after.line_start_of(last));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Copy { lines, to } => {
                let range = self.lines(host, lines.as_ref(), &CURRENT)?;
                let buf = Text::new(&*host);
                let current = buf.line_of(host.cursor());
                let dest = self.address(host, &to, current)?;
                let start = buf.line_start_of(range.start);
                let end = buf.line_end(buf.line_start_of(range.end - 1));
                let text = host.slice(start..end).into_owned();
                let n = range.len();
                let tx = if dest < 0 {
                    Transaction::insert(0, format!("{text}\n"), 0)
                } else {
                    let le = buf.line_end(buf.line_start_of(dest as usize));
                    Transaction::insert(le, format!("\n{text}"), le + 1)
                };
                host.begin_group();
                self.apply(host, tx);
                let last = (dest + n as isize) as usize;
                let after = Text::new(&*host);
                let to = after.first_nonblank(after.line_start_of(last));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Join {
                lines,
                count,
                keep_spaces,
            } => {
                let range = match (&lines, count) {
                    (_, Some(_)) => self.counted(host, lines.as_ref(), count)?,
                    (Some(LineSpec::Two { .. } | LineSpec::All), None) => {
                        self.lines(host, lines.as_ref(), &CURRENT)?
                    }
                    _ => {
                        let r = self.lines(host, lines.as_ref(), &CURRENT)?;
                        r.start..r.start + 2
                    }
                };
                let last = range.end.min(Text::new(&*host).line_count()) - 1;
                if last <= range.start {
                    return Ok(());
                }
                self.join(host, range.start, last, !keep_spaces);
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(range.start));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Shift {
                lines,
                left,
                amount,
                count,
            } => {
                let range = self.counted(host, lines.as_ref(), count)?;
                host.begin_group();
                self.shift_lines(host, range.start, range.end - 1, left, amount);
                let buf = Text::new(&*host);
                let to = buf.first_nonblank(buf.line_start_of(range.end - 1));
                host.set_cursor(to);
                Ok(())
            }
            Ex::Mark { line, name } => {
                let buf = Text::new(&*host);
                let current = buf.line_of(host.cursor());
                let line = match &line {
                    Some(at) => self.line(host, at, current)?,
                    None => current,
                };
                self.marks.set(name, Pos { line, col: 0 });
                Ok(())
            }
            Ex::Undo => {
                if !host.undo() {
                    return Err(ExError("Already at oldest change".into()));
                }
                Ok(())
            }
            Ex::Redo => {
                if !host.redo() {
                    return Err(ExError("Already at newest change".into()));
                }
                Ok(())
            }
            other => Err(not_supported(owner_name(&other))),
        }
    }

    fn ex_store(
        &mut self,
        register: Option<char>,
        reg: Register,
        write: Write,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        match register {
            Some('_') => {}
            Some('+' | '*') => clip.write(&reg.clipboard_form()),
            None | Some('"') => {
                if opts.use_system_clipboard {
                    clip.write(&reg.clipboard_form());
                }
                self.regs.store(None, reg, write);
            }
            Some(name) => self.regs.store(Some(name), reg, write),
        }
    }

    fn set_options(&mut self, options: &[SetOption]) -> Result<(), ExError> {
        let mut answers = Vec::new();
        for option in options {
            let s = &mut self.settings;
            let flag = match option.name {
                OptionName::Number => return Err(ExError("Not supported: :set number".into())),
                OptionName::IgnoreCase => &mut s.ignore_case,
                OptionName::SmartCase => &mut s.smart_case,
                OptionName::WrapScan => &mut s.wrap_scan,
                OptionName::HlSearch => &mut s.hl_search,
                OptionName::IncSearch => &mut s.inc_search,
                OptionName::ExpandTab => &mut s.expand_tab,
                OptionName::ShiftRound => &mut s.shift_round,
                OptionName::JoinSpaces => &mut s.join_spaces,
                OptionName::ShiftWidth | OptionName::TabStop => {
                    let value = if option.name == OptionName::ShiftWidth {
                        &mut s.shift_width
                    } else {
                        &mut s.tab_stop
                    };
                    match option.toggle {
                        Toggle::Value(n) => *value = n,
                        _ => answers.push(format!("  {}={}", option.name.name(), value)),
                    }
                    continue;
                }
            };
            match option.toggle {
                Toggle::On => *flag = true,
                Toggle::Off => *flag = false,
                Toggle::Invert => *flag = !*flag,
                Toggle::Query => answers.push(if *flag {
                    format!("  {}", option.name.name())
                } else {
                    format!("no{}", option.name.name())
                }),
                Toggle::Value(_) => {}
            }
        }
        if !answers.is_empty() {
            self.say(answers.join(" "));
        }
        Ok(())
    }

    // ---- :s --------------------------------------------------------------------------------------

    pub fn prepare_substitute(&mut self, command: &mut Substitute) -> Result<Pattern, ExError> {
        let repeat = command.replacement.is_none();
        let last = self.last_sub.clone();
        if repeat {
            let last = last
                .clone()
                .ok_or_else(|| ExError("E35: No previous regular expression".into()))?;
            command.replacement = Some(last.replacement.clone());
            if command.keep_flags {
                command.global = command.global != last.global;
                command.ignore_case = command.ignore_case.or(last.ignore_case);
            }
        }
        let pattern = if command.pattern.is_empty() {
            let from = if repeat {
                last.as_ref().map(|l| l.pattern.clone())
            } else {
                self.last_search.as_ref().map(|s| s.pattern.clone())
            };
            from.ok_or_else(|| ExError("E35: No previous regular expression".into()))?
        } else {
            command.pattern.clone()
        };
        command.pattern = pattern.clone();
        let compiled = Pattern::new(
            &pattern,
            command.ignore_case,
            self.settings.case(),
            self.last_replacement.as_deref(),
        )?;
        if !repeat {
            let replacement = with_previous(
                command.replacement.as_deref().unwrap_or(""),
                self.last_replacement.as_deref(),
            );
            self.last_replacement = Some(replacement.clone());
            command.replacement = Some(replacement);
        }
        self.last_search = Some(LastSearch {
            pattern: pattern.clone(),
            forward: self.last_search.as_ref().is_none_or(|s| s.forward),
            offset: String::new(),
        });
        self.regs.last_search = Some(pattern.clone());
        self.last_sub = Some(LastSubstitute {
            pattern,
            replacement: command.replacement.clone().unwrap_or_default(),
            global: command.global,
            ignore_case: command.ignore_case,
        });
        Ok(compiled)
    }

    pub fn substitute<H: VimHost + ?Sized>(
        &mut self,
        host: &mut H,
        lines: Option<&LineSpec>,
        mut command: Substitute,
    ) -> Result<(), ExError> {
        let range = self.counted(host, lines, command.count)?;
        let pattern = self.prepare_substitute(&mut command)?;
        let replacement = command.replacement.clone().unwrap_or_default();
        let buf = Text::new(&*host);
        let mut edits: Vec<Edit> = Vec::new();
        let mut matches = 0;
        let mut changed_lines = 0;
        let mut last_line = None;
        let mut breaks_before_last = 0;
        for line in range {
            let ls = buf.line_start_of(line);
            let text = buf.line_text(line);
            let found = pattern.replacements(&text, &replacement, command.global);
            if found.is_empty() {
                continue;
            }
            matches += found.len();
            changed_lines += 1;
            last_line = Some(line);
            for (r, new) in found {
                breaks_before_last += new.matches('\n').count();
                edits.push(Edit {
                    range: ls + r.start..ls + r.end,
                    text: new,
                });
            }
        }
        let Some(last_line) = last_line else {
            if command.quiet || self.line_lists_active() {
                return Ok(());
            }
            return Err(ExError(format!(
                "E486: Pattern not found: {}",
                command.pattern
            )));
        };
        if command.count_only {
            self.say(format!(
                "{matches} match{} on {changed_lines} line{}",
                if matches == 1 { "" } else { "es" },
                if changed_lines == 1 { "" } else { "s" }
            ));
            return Ok(());
        }
        self.changed = true;
        let cursor = edits.first().map_or(0, |e| e.range.start);
        host.begin_group();
        self.apply(host, Transaction { edits, cursor });
        let after = Text::new(&*host);
        let line = (last_line + breaks_before_last).min(after.line_count() - 1);
        let to = after.first_nonblank(after.line_start_of(line));
        host.set_cursor(to);
        self.marks.last_change = Some(Self::pos(host, to));
        if changed_lines > 2 && !self.line_lists_active() {
            self.say(format!(
                "{matches} substitution{} on {changed_lines} lines",
                if matches == 1 { "" } else { "s" }
            ));
        }
        Ok(())
    }

    fn line_lists_active(&self) -> bool {
        !self.line_lists.is_empty()
    }

    // ---- :g and :normal ----------------------------------------------------------------------------

    /// The pattern of a `:g` (the last search pattern for an empty one), remembered as the last
    /// search and compiled.
    pub fn global_pattern(&mut self, pattern: &str) -> Result<Pattern, ExError> {
        let pattern = if pattern.is_empty() {
            self.last_search
                .as_ref()
                .map(|s| s.pattern.clone())
                .ok_or_else(|| ExError("E35: No previous regular expression".into()))?
        } else {
            pattern.to_owned()
        };
        let compiled = Pattern::new(
            &pattern,
            None,
            self.settings.case(),
            self.last_replacement.as_deref(),
        )?;
        self.last_search = Some(LastSearch {
            pattern: pattern.clone(),
            forward: true,
            offset: String::new(),
        });
        self.regs.last_search = Some(pattern);
        Ok(compiled)
    }

    #[allow(clippy::too_many_arguments)]
    fn global<H: VimHost>(
        &mut self,
        host: &mut H,
        lines: Option<&LineSpec>,
        invert: bool,
        pattern: &str,
        command: Ex,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) -> Result<(), ExError> {
        let range = self.lines(host, lines, &LineSpec::All)?;
        let compiled = self.global_pattern(pattern)?;
        let buf = Text::new(&*host);
        let marked: Vec<Option<usize>> = range
            .filter(|&l| compiled.is_match(&buf.line_text(l)) != invert)
            .map(Some)
            .collect();
        if marked.is_empty() {
            let pattern = self
                .last_search
                .as_ref()
                .map(|s| s.pattern.clone())
                .unwrap_or_default();
            return Err(ExError(if invert {
                format!("Pattern found in every line: {pattern}")
            } else {
                format!("E486: Pattern not found: {pattern}")
            }));
        }
        host.begin_group();
        self.depth += 1;
        self.line_lists.push(marked);
        let mut i = 0;
        let mut result = Ok(());
        loop {
            let next = self.line_lists.last().and_then(|l| l.get(i).copied());
            let Some(entry) = next else { break };
            i += 1;
            let Some(line) = entry else { continue };
            let buf = Text::new(&*host);
            if line >= buf.line_count() {
                continue;
            }
            let ls = buf.line_start_of(line);
            host.set_cursor(ls);
            if let Err(e) = self.run_ex(host, command.clone(), opts, clip) {
                result = Err(e);
                break;
            }
        }
        self.line_lists.pop();
        self.depth -= 1;
        self.clamp_normal(host);
        result
    }

    /// `:normal keys`, at the caret (`lines` none) or from the start of each line, in one undo
    /// step; lines removed by the keys of an earlier line are skipped.
    pub fn normal_on_lines<H: VimHost>(
        &mut self,
        host: &mut H,
        lines: Option<&[usize]>,
        keys: &str,
        opts: Options,
        clip: &mut dyn Clipboard,
    ) {
        let keys = text_keys(keys);
        host.begin_group();
        self.depth += 1;
        self.settle(host);
        let list = match lines {
            Some(lines) => lines.iter().copied().map(Some).collect(),
            None => vec![None],
        };
        self.line_lists.push(list);
        let mut i = 0;
        loop {
            let next = self.line_lists.last().and_then(|l| l.get(i).copied());
            let Some(entry) = next else { break };
            i += 1;
            if lines.is_some() {
                let Some(line) = entry else { continue };
                let buf = Text::new(&*host);
                if line >= buf.line_count() {
                    continue;
                }
                let ls = buf.line_start_of(line);
                host.set_cursor(ls);
            }
            self.failed = false;
            self.feed(host, &keys, opts, clip);
            self.settle(host);
        }
        self.line_lists.pop();
        self.depth -= 1;
        self.failed = false;
    }

    /// Ends whatever keys left unfinished (Insert, Visual, a pending command, a `:` line) as
    /// Escape would.
    fn settle<H: VimHost>(&mut self, host: &mut H) {
        self.cmdline = None;
        self.pending.clear();
        if self.mode == Mode::Insert {
            let opts = Options {
                insert_passthrough: false,
                ..Options::default()
            };
            self.leave_insert(host, opts);
        }
        if self.mode.is_visual() {
            self.leave_visual(host);
        }
        self.rec.clear();
        self.changed = false;
    }
}
