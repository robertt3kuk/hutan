//! Vim in the code view (slice E-8): the modal layer of `hutan-vim` over the file's rope
//! ([`BufferHost`]), one Vim state per view.
//!
//! - **Modes.** A file opens in Normal mode (block caret); Insert has a bar caret and types
//!   through the platform's text input like the editor without Vim (auto-pairs, auto-indent,
//!   input methods); Visual draws its selection from the Vim state. In Normal and Visual the
//!   view refuses text input (`accepts_text_input` is false), so printable keys arrive as key
//!   presses and are matched by the character they produce (a Dvorak or Cyrillic layout types
//!   its own letters as commands), and never insert text, an input method or not.
//! - **Escape** in Insert first ends an input method's composition (the marked text stays);
//!   only the next Escape leaves Insert.
//! - **Keys.** The view's key context carries `vim_mode = normal | insert | visual`; the
//!   editor's own bindings that would shadow a Vim key are bound outside Normal and Visual
//!   (see the desktop's keymap). Every key that reaches the view's key handler goes to Vim
//!   in Normal and Visual; in Insert only Escape does.
//! - **The `:` line** is the owner's (the desktop draws it): `:` asks for it
//!   ([`CodeViewEvent::CommandLine`]); [`CodeView::run_ex`] runs what concerns the text and
//!   hands the rest (files, windows, terminals) back.
//! - **Undo.** A Normal command, and an Insert session with the command that started it, is one
//!   undo step; `:s`, `:g` and `:normal` are one step each.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AppContext as _, ClipboardItem, Context, KeyDownEvent};
use hutan_buffer::{Bias, Selection, Selections, Text, Transaction};
use hutan_vim::{Clipboard, Key, Mode, Options, Outcome, Vim, VimHost as _};

use crate::buffer::Coalesce;
use crate::ex::{
    self, ALL_LINES, CURRENT_LINE, CaseOptions, Ex, ExError, GlobalCommand, LineSpec, Resolve,
    SetOption, Substitute, Toggle,
};
use crate::view::{CodeView, CodeViewEvent};
use crate::vim_host::BufferHost;

/// How Vim behaves in a view: what the desktop's `editor.*` settings say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VimSettings {
    /// Yanks and deletes also write the system clipboard, and `p` pastes from it.
    pub use_system_clipboard: bool,
}

/// A line under the code: the answer of a `:` command, or an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimMessage {
    pub text: String,
    pub error: bool,
}

/// The Vim state of one view.
#[derive(Debug)]
pub(crate) struct VimState {
    vim: Vim<BufferHost>,
    options: Options,
    /// The Vim undo group the last key left open (an Insert session).
    group_open: bool,
    /// The Visual selection as drawn (bytes, the character under the caret included).
    visual: Option<Range<usize>>,
    /// The lines (0-based) of the last Visual selection: `'<` and `'>`.
    last_visual: Option<(usize, usize)>,
    case: CaseOptions,
    last_pattern: Option<String>,
    message: Option<VimMessage>,
}

/// What [`CodeView::run_ex`] did with a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExRun {
    /// Ran (or started, for `:s` and `:g`: [`CodeView::ex_running`]); any answer is the view's
    /// [`vim_message`](CodeView::vim_message).
    Done,
    /// Not the view's: files, windows, buffers, terminals and the find highlights are the
    /// owner's.
    Owner(Ex),
}

/// The key the modal layer sees for `event`; `None` for ⌘ and fn combinations (shortcuts, not
/// typing) and for keys it has no name for. Printable keys are matched by the character they
/// produce.
pub fn vim_key_of(event: &KeyDownEvent) -> Option<Key> {
    let ks = &event.keystroke;
    let mods = &ks.modifiers;
    if mods.platform || mods.function {
        return None;
    }
    let named = match ks.key.as_str() {
        "escape" => Some(Key::Escape),
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        _ => None,
    };
    if let Some(key) = named {
        return (!mods.control && !mods.alt).then_some(key);
    }
    if mods.control {
        let mut chars = ks.key.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() && !mods.alt => Some(Key::Ctrl(c)),
            _ => None,
        };
    }
    let text = ks.key_char.as_deref()?;
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(Key::Char(c)),
        _ => None,
    }
}

struct AppClipboard<'a>(&'a mut App);

impl Clipboard for AppClipboard<'_> {
    fn read(&mut self) -> Option<String> {
        self.0.read_from_clipboard().and_then(|item| item.text())
    }

    fn write(&mut self, text: &str) {
        self.0
            .write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
    }
}

/// The 0-based line of byte `at`.
fn line_of(text: &Text, at: usize) -> usize {
    text.line_of_byte(text.floor_char_boundary(at)).unwrap_or(0)
}

/// The first character of `line` that is not a blank.
fn first_nonblank(text: &Text, line: usize) -> usize {
    let Ok(content) = text.line_content_range(line) else {
        return text.len();
    };
    let mut at = content.start;
    while at < content.end {
        match text.char_at(at) {
            Some(c @ (' ' | '\t')) => at += c.len_utf8(),
            _ => break,
        }
    }
    at
}

impl CodeView {
    // ---- state -------------------------------------------------------------------------------

    /// Turns Vim on (`Some`) or off, live. Safe in the middle of a command: a pending command is
    /// dropped, an Insert session's undo step is closed, and a view that turns Vim on is in
    /// Normal mode with its caret on a character.
    pub fn set_vim(&mut self, settings: Option<VimSettings>, cx: &mut Context<Self>) {
        match (settings, self.vim.is_some()) {
            (Some(settings), true) => {
                if let Some(state) = &mut self.vim {
                    state.options.use_system_clipboard = settings.use_system_clipboard;
                }
                return;
            }
            (None, false) => return,
            (Some(settings), false) => {
                self.end_composition(cx);
                self.vim = Some(VimState {
                    vim: Vim::new(),
                    options: Options {
                        single_line: false,
                        insert_passthrough: true,
                        use_system_clipboard: settings.use_system_clipboard,
                    },
                    group_open: false,
                    visual: None,
                    last_visual: None,
                    case: CaseOptions::default(),
                    last_pattern: None,
                    message: None,
                });
                // A new state is in Insert: Escape takes it to Normal like any other.
                self.vim_key(Key::Escape, cx);
            }
            (None, true) => {
                if let Some(state) = self.vim.take()
                    && state.group_open
                {
                    let editable = self.is_editable(cx);
                    let cursor = self.selections.primary().head;
                    let id = cx.entity_id();
                    let model = self.buffer().update(cx, |b, _| b.lend());
                    let mut host = BufferHost::new(model, cursor, editable, true);
                    host.end_group();
                    let (model, _, _, steps) = host.finish();
                    self.buffer()
                        .update(cx, |b, cx| b.restore(model, steps, id, cx));
                }
                self.ex_task = None;
            }
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// The Vim mode, while Vim is on.
    pub fn vim_mode(&self) -> Option<Mode> {
        self.vim.as_ref().map(|v| v.vim.mode())
    }

    /// The keys typed so far of a command in progress (`2d`), for the pending-keys display.
    pub fn vim_pending(&self) -> String {
        self.vim
            .as_ref()
            .map(|v| v.vim.pending_keys())
            .unwrap_or_default()
    }

    /// The answer of the last `:` command, until the next key.
    pub fn vim_message(&self) -> Option<&VimMessage> {
        self.vim.as_ref().and_then(|v| v.message.as_ref())
    }

    /// Shows `text` under the code (the owner's answer to a command it ran).
    pub fn set_vim_message(
        &mut self,
        text: impl Into<String>,
        error: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.vim {
            state.message = Some(VimMessage {
                text: text.into(),
                error,
            });
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
        }
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        if let Some(state) = &mut self.vim {
            state.message = Some(VimMessage {
                text: text.into(),
                error,
            });
        }
    }

    /// Whether `:set number` is on (the gutter's line numbers).
    pub fn shows_numbers(&self) -> bool {
        self.show_numbers
    }

    /// The selection to draw: the Visual selection in Visual mode, else the carets' selections.
    pub(crate) fn drawn_selections(&self) -> Vec<Range<usize>> {
        match self.vim.as_ref().and_then(|v| v.visual.clone()) {
            Some(visual) => vec![visual],
            None => self.selections.iter().map(Selection::range).collect(),
        }
    }

    /// Whether the caret is drawn as a block (Normal and Visual).
    pub(crate) fn block_caret(&self) -> bool {
        self.vim_mode().is_some_and(Mode::block_caret)
    }

    /// Whether the platform's text input may type into the view: not in Normal or Visual.
    pub(crate) fn vim_accepts_text(&self) -> bool {
        self.vim_mode().is_none_or(|m| m == Mode::Insert)
    }

    // ---- keys --------------------------------------------------------------------------------

    /// A key press that no binding took. Returns whether the view used it.
    pub(crate) fn vim_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(mode) = self.vim_mode() else {
            return false;
        };
        let Some(key) = vim_key_of(event) else {
            return false;
        };
        if mode == Mode::Insert {
            if key != Key::Escape {
                return false;
            }
            // An input method's composition ends first; the next Escape leaves Insert.
            if self.end_composition(cx) {
                return true;
            }
        }
        if mode != Mode::Insert && key == Key::Char(':') && self.vim_pending().is_empty() {
            self.open_command_line(cx);
            return true;
        }
        self.vim_key(key, cx) == Outcome::Handled
    }

    /// `:` in Normal or Visual: Visual ends (its lines become `'<` and `'>`) and the owner opens
    /// the command line.
    fn open_command_line(&mut self, cx: &mut Context<Self>) {
        let visual = self.vim_mode() == Some(Mode::Visual);
        if visual {
            self.vim_key(Key::Escape, cx);
        }
        if let Some(state) = &mut self.vim {
            state.message = None;
        }
        cx.emit(CodeViewEvent::CommandLine { visual });
        cx.notify();
    }

    /// Applies one key to Vim (Normal, Visual, or Escape in Insert): the text model is lent to a
    /// [`BufferHost`] for the key and taken back with what it did.
    pub fn vim_key(&mut self, key: Key, cx: &mut Context<Self>) -> Outcome {
        // Before the file is in, its text is empty and read-only: a key changes only the mode
        // (a new state goes to Normal this way while its file loads).
        if self.vim.is_none() {
            return Outcome::Ignored;
        }
        let editable = self.is_editable(cx);
        let id = cx.entity_id();
        let primary = self.selections.primary();
        let model = self.buffer().update(cx, |b, _| b.lend());
        let Some(state) = self.vim.as_mut() else {
            return Outcome::Ignored;
        };
        state.message = None;
        let mut host = BufferHost::new(model, primary.head, editable, state.group_open);
        let mut clip = AppClipboard(cx);
        // A selection made with the mouse in Normal mode becomes a Visual one.
        if state.vim.mode() == Mode::Normal && !primary.is_empty() && key != Key::Escape {
            host.set_cursor(primary.anchor);
            state
                .vim
                .key(&mut host, Key::Char('v'), state.options, &mut clip);
            let head = if primary.head > primary.anchor {
                host.text()
                    .char_before(primary.head)
                    .map_or(primary.head, |c| primary.head - c.len_utf8())
            } else {
                primary.head
            };
            host.set_cursor(head);
        }
        let outcome = state.vim.key(&mut host, key, state.options, &mut clip);
        state.visual = state.vim.selection(&host);
        if let Some(visual) = &state.visual {
            let text = host.text();
            state.last_visual = Some((
                line_of(text, visual.start),
                line_of(text, visual.end.saturating_sub(1).max(visual.start)),
            ));
        }
        let (model, cursor, open, steps) = host.finish();
        state.group_open = open;
        self.buffer()
            .update(cx, |b, cx| b.restore(model, steps, id, cx));
        self.selections = Selections::cursor(cursor);
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        outcome
    }

    // ---- the `:` line ------------------------------------------------------------------------

    /// Whether a `:s` or `:g` is still searching (tests wait on it).
    pub fn ex_running(&self) -> bool {
        self.ex_task.is_some()
    }

    /// Parses and runs one `:` line. What concerns the text runs here; the rest is handed back.
    pub fn run_command_line(&mut self, line: &str, cx: &mut Context<Self>) -> ExRun {
        match ex::parse(line) {
            Ok(Some(command)) => self.run_ex(command, cx),
            Ok(None) => ExRun::Done,
            Err(error) => {
                self.set_vim_message(error.0, true, cx);
                ExRun::Done
            }
        }
    }

    /// Runs `command` if it concerns the text (`:s :g :v :normal :set :N`).
    pub fn run_ex(&mut self, command: Ex, cx: &mut Context<Self>) -> ExRun {
        let result = match command {
            Ex::Set(options) => {
                self.set_options(&options);
                Ok(())
            }
            Ex::Goto(at) => self
                .resolve(cx)
                .line(at)
                .map(|line| self.goto_line(line, cx)),
            Ex::Substitute { lines, command } => self.substitute(lines, command, cx),
            Ex::Global {
                lines,
                invert,
                pattern,
                command,
            } => self.global(lines, invert, &pattern, command, cx),
            Ex::Normal { lines, keys } => self
                .resolve(cx)
                .lines(lines, CURRENT_LINE)
                .map(|lines| self.normal_on_lines(lines.collect(), &keys, cx)),
            other => return ExRun::Owner(other),
        };
        if let Err(error) = result {
            self.say(error.0, true);
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        ExRun::Done
    }

    fn resolve(&self, cx: &App) -> Resolve {
        let text = self.buffer().read(cx).text();
        Resolve {
            current: line_of(text, self.selections.primary().head),
            line_count: text.line_count(),
            visual: self.vim.as_ref().and_then(|v| v.last_visual),
        }
    }

    fn set_options(&mut self, options: &[SetOption]) {
        let mut answers = Vec::new();
        for option in options {
            let (name, value, toggle) = match option {
                SetOption::Number(t) => ("number", &mut self.show_numbers, *t),
                SetOption::IgnoreCase(t) => match &mut self.vim {
                    Some(state) => ("ignorecase", &mut state.case.ignore_case, *t),
                    None => continue,
                },
                SetOption::SmartCase(t) => match &mut self.vim {
                    Some(state) => ("smartcase", &mut state.case.smart_case, *t),
                    None => continue,
                },
            };
            match toggle {
                Toggle::On => *value = true,
                Toggle::Off => *value = false,
                Toggle::Invert => *value = !*value,
                Toggle::Query => answers.push(if *value {
                    format!("  {name}")
                } else {
                    format!("no{name}")
                }),
            }
        }
        // The gutter changes width: every line is drawn again.
        self.lines.clear();
        if !answers.is_empty() {
            self.say(answers.join(" "), false);
        }
    }

    /// The caret to the first non-blank of 0-based `line`, brought into view.
    fn goto_line(&mut self, line: usize, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let at = first_nonblank(&text, line);
        self.selections = Selections::cursor(at);
        self.goal = None;
        self.buffer().update(cx, |b, _| b.break_group());
        self.reveal_head(cx);
    }

    /// The pattern of `:s` / `:g`: the one given, else the last one (Vim's `:s//x/`).
    fn pattern(&mut self, given: &str) -> Result<String, ExError> {
        let state = self
            .vim
            .as_mut()
            .ok_or_else(|| ExError("Vim mode is off".into()))?;
        if given.is_empty() {
            return state
                .last_pattern
                .clone()
                .ok_or_else(|| ExError("E35: No previous regular expression".into()));
        }
        state.last_pattern = Some(given.to_owned());
        Ok(given.to_owned())
    }

    fn case(&self) -> CaseOptions {
        self.vim.as_ref().map(|v| v.case).unwrap_or_default()
    }

    /// `:s`: searched in the background over a snapshot of the rope, applied as one
    /// transaction if the text did not change meanwhile.
    fn substitute(
        &mut self,
        lines: Option<LineSpec>,
        mut command: Substitute,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let range = self.resolve(cx).lines(lines, CURRENT_LINE)?;
        command.pattern = self.pattern(&command.pattern)?;
        let regex = ex::compile(&command.pattern, command.ignore_case, self.case())?;
        let (text, version) = self.snapshot(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        let job = {
            let cancel = cancel.clone();
            let command = command.clone();
            cx.background_spawn(async move {
                ex::substitute(&text, range, &regex, &command, &|| {
                    cancel.load(Ordering::Relaxed)
                })
            })
        };
        self.start_ex(cancel, cx, async move |this, cx| {
            let found = job.await;
            this.update(cx, |this, cx| {
                this.finish_substitute(found, version, &command, cx)
            })
            .ok();
        });
        Ok(())
    }

    fn snapshot(&self, cx: &App) -> (Text, u64) {
        let buffer = self.buffer().read(cx);
        (buffer.text().clone(), buffer.version())
    }

    /// Runs `body` as the one `:` job of the view (a newer one cancels it).
    fn start_ex(
        &mut self,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
        body: impl AsyncFnOnce(gpui::WeakEntity<Self>, &mut gpui::AsyncApp) + 'static,
    ) {
        if let Some(previous) = self.ex_cancel.replace(cancel) {
            previous.store(true, Ordering::Relaxed);
        }
        self.ex_task = Some(cx.spawn(async move |this, cx| {
            body(this.clone(), cx).await;
            this.update(cx, |this, cx| {
                this.ex_task = None;
                this.ex_cancel = None;
                cx.emit(CodeViewEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether the text is still the `version` a `:` job read; says so when it is not.
    fn still(&mut self, version: u64, cx: &App) -> bool {
        if self.buffer().read(cx).version() == version {
            return true;
        }
        self.say(
            "The text changed while the command ran; nothing was changed",
            true,
        );
        false
    }

    fn finish_substitute(
        &mut self,
        found: ex::Substitution,
        version: u64,
        command: &Substitute,
        cx: &mut Context<Self>,
    ) {
        if !self.still(version, cx) {
            return;
        }
        if found.matches == 0 {
            if !command.quiet {
                self.say(
                    format!("E486: Pattern not found: {}", command.pattern),
                    true,
                );
            }
            return;
        }
        if command.count_only {
            self.say(
                count_message(found.matches, found.lines, "match", "matches"),
                false,
            );
            return;
        }
        let Some(last_line) = found.last_line else {
            return;
        };
        if self.apply_lines_edit(found.edits, last_line, cx) && found.matches > 2 {
            self.say(
                count_message(found.matches, found.lines, "substitution", "substitutions"),
                false,
            );
        }
    }

    /// Applies `edits` as one undo step and puts the caret on the first non-blank of what was
    /// `line` before them.
    fn apply_lines_edit(
        &mut self,
        edits: Vec<hutan_buffer::Edit>,
        line: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = self.buffer().read(cx).text().clone();
        let start = text.line_start(line).unwrap_or(0);
        let Ok(transaction) = Transaction::new(edits.clone()) else {
            return false;
        };
        let at = transaction.map_offset(start, Bias::Left);
        if !self.commit(edits, Selections::cursor(at), Coalesce::Step, cx) {
            self.say("The file cannot be changed", true);
            return false;
        }
        let text = self.buffer().read(cx).text().clone();
        let line = line_of(&text, at);
        self.selections = Selections::cursor(first_nonblank(&text, line));
        self.reveal_head(cx);
        true
    }

    /// `:g/pat/cmd` and `:v/pat/cmd` (`invert`): the matching lines are found in the background;
    /// `d` and `s` are one transaction, `normal` runs its keys on every line, in one undo step.
    fn global(
        &mut self,
        lines: Option<LineSpec>,
        invert: bool,
        pattern: &str,
        mut command: GlobalCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let range = self.resolve(cx).lines(lines, ALL_LINES)?;
        let pattern = self.pattern(pattern)?;
        let regex = ex::compile(&pattern, None, self.case())?;
        let sub = match &mut command {
            GlobalCommand::Substitute(sub) => {
                if sub.pattern.is_empty() {
                    sub.pattern = pattern.clone();
                }
                Some(ex::compile(&sub.pattern, sub.ignore_case, self.case())?)
            }
            _ => None,
        };
        let (text, version) = self.snapshot(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        let job = {
            let cancel = cancel.clone();
            let command = command.clone();
            cx.background_spawn(async move {
                let cancelled = || cancel.load(Ordering::Relaxed);
                let lines = ex::matching_lines(&text, range, &regex, invert, &cancelled);
                let edits = match (&command, &sub) {
                    (GlobalCommand::Delete, _) => Some(ex::Substitution {
                        edits: ex::delete_lines(&text, &lines),
                        matches: lines.len(),
                        lines: lines.len(),
                        last_line: lines.first().copied(),
                    }),
                    (GlobalCommand::Substitute(s), Some(sub)) => {
                        let mut all = ex::Substitution::default();
                        for &line in &lines {
                            let one = ex::substitute(&text, line..line + 1, sub, s, &cancelled);
                            all.edits.extend(one.edits);
                            all.matches += one.matches;
                            all.lines += one.lines;
                            all.last_line = one.last_line.or(all.last_line);
                        }
                        Some(all)
                    }
                    _ => None,
                };
                (lines, edits)
            })
        };
        self.start_ex(cancel, cx, async move |this, cx| {
            let (lines, edits) = job.await;
            this.update(cx, |this, cx| {
                if !this.still(version, cx) {
                    return;
                }
                if lines.is_empty() {
                    this.say(format!("E486: Pattern not found: {pattern}"), true);
                    return;
                }
                match (command, edits) {
                    (GlobalCommand::Normal(keys), _) => this.normal_on_lines(lines, &keys, cx),
                    (GlobalCommand::Delete, Some(found)) => {
                        let line = found.last_line.unwrap_or(0);
                        if this.apply_lines_edit(found.edits, line, cx) && found.lines > 2 {
                            this.say(format!("{} fewer lines", found.lines), false);
                        }
                    }
                    (GlobalCommand::Substitute(sub), Some(found)) => {
                        this.finish_substitute(found, version, &sub, cx);
                    }
                    _ => {}
                }
            })
            .ok();
        });
        Ok(())
    }

    /// `:normal keys` on each of `lines` (0-based, in order), as one undo step: the caret goes
    /// to the start of the line, the keys run as typed in Normal mode (typed text included),
    /// and whatever they leave unfinished (Insert, Visual, a pending operator) ends as with
    /// Escape.
    fn normal_on_lines(&mut self, lines: Vec<usize>, keys: &str, cx: &mut Context<Self>) {
        if self.vim.is_none() || !self.buffer().read(cx).is_ready() {
            return;
        }
        let editable = self.is_editable(cx);
        let id = cx.entity_id();
        let cursor = self.selections.primary().head;
        let mut model = self.buffer().update(cx, |b, _| b.lend());
        let mut starts: Vec<usize> = lines
            .iter()
            .filter_map(|line| model.text().line_start(*line).ok())
            .collect();
        // One undo step around every command the keys run (the buffer's groups nest).
        model.begin_group();
        let Some(state) = self.vim.as_mut() else {
            return;
        };
        let mut host = BufferHost::new(model, cursor, editable, false);
        let mut clip = AppClipboard(cx);
        let options = Options {
            insert_passthrough: false,
            ..state.options
        };
        settle(&mut state.vim, &mut host, options, &mut clip);
        for i in 0..starts.len() {
            host.set_cursor(starts[i]);
            let done = host.step_count();
            for c in keys.chars() {
                state.vim.key(&mut host, Key::Char(c), options, &mut clip);
            }
            settle(&mut state.vim, &mut host, options, &mut clip);
            // The lines below follow what the keys changed.
            for transaction in host.transactions_since(done) {
                for start in &mut starts[i + 1..] {
                    *start = transaction.map_offset(*start, Bias::Left);
                }
            }
        }
        state.visual = None;
        let (mut model, cursor, open, steps) = host.finish();
        if open {
            model.end_group();
        }
        model.end_group();
        state.group_open = false;
        self.buffer()
            .update(cx, |b, cx| b.restore(model, steps, id, cx));
        self.selections = Selections::cursor(cursor);
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
    }
}

fn count_message(n: usize, lines: usize, one: &str, many: &str) -> String {
    format!(
        "{n} {} on {lines} line{}",
        if n == 1 { one } else { many },
        if lines == 1 { "" } else { "s" }
    )
}

/// Ends whatever keys left unfinished (Insert, Visual, a pending operator) as Escape would.
fn settle(
    vim: &mut Vim<BufferHost>,
    host: &mut BufferHost,
    options: Options,
    clip: &mut dyn Clipboard,
) {
    for _ in 0..3 {
        if vim.mode() == Mode::Normal && vim.pending_keys().is_empty() {
            break;
        }
        vim.key(host, Key::Escape, options, clip);
    }
}
