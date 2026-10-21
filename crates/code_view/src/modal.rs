//! Vim in the code view (slices E-8, E-7b): the modal layer of `hutan-vim` over the file's rope
//! ([`BufferHost`]), one Vim state per view.
//!
//! - **Modes.** A file opens in Normal mode (block caret); Insert has a bar caret and types
//!   through the platform's text input like the editor without Vim (auto-pairs, auto-indent,
//!   input methods); Visual (charwise, linewise, blockwise) draws its selection from the Vim
//!   state, one range per line for a block. In Normal and Visual the view refuses text input
//!   (`accepts_text_input` is false), so printable keys arrive as key presses and are matched by
//!   the character they produce (a Dvorak layout types its own letters as commands), a
//!   character that is not ASCII by the key's place on the keyboard (a Russian layout's `в` key
//!   is `d`; the character itself after `f`, `t`, `r` and on the search line: [`vim_key_of`]),
//!   and never insert text, an input method or not. A click in Normal mode puts the caret on a
//!   character, never past the end of a line.
//! - **Escape** in Insert first ends an input method's composition (the marked text stays);
//!   only the next Escape leaves Insert.
//! - **Keys.** The view's key context carries `vim_mode = normal | insert | visual` (all three
//!   Visual modes are `visual`); the editor's own bindings that would shadow a Vim key are bound
//!   outside Normal and Visual (see the desktop's keymap). Every key that reaches the view's
//!   key handler goes to Vim in Normal and Visual; in Insert only Escape does.
//! - **The `:` line** is the owner's (the desktop draws it): `:` asks for it
//!   ([`CodeViewEvent::CommandLine`]); [`CodeView::run_command_line`] parses it with
//!   `hutan_vim::ex`, runs `:s` and `:g` (with `d`, `s` or `normal`) on a background thread,
//!   every other text command through the Vim state, and hands the rest (files, windows,
//!   terminals, `:noh`) back. `/` and `?` are typed into the Vim state itself; the owner shows
//!   the line being typed under the code ([`CodeView::vim_command_line`]).
//! - **Indentation.** `'tabstop'` is the view's tab width and `'shiftwidth'` / `'expandtab'`
//!   follow the file's indentation, until a `:set` of one of them.
//! - **Undo.** A Normal command, and an Insert session with the command that started it, is one
//!   undo step; a macro, `:s`, `:g` and `:normal` are one step each.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AppContext as _, ClipboardItem, Context, KeyDownEvent};
use hutan_buffer::{Bias, Selection, Selections, Text, Transaction};
use hutan_vim::ex::{
    self as vim_ex, Ex, ExError, LineRef, LineSpec, OptionName, SetOption, Substitute, Toggle,
};
use hutan_vim::{Clipboard, Key, Mode, Options, Outcome, Vim, VimHost as _};

use crate::buffer::Coalesce;
use crate::ex;
use crate::layout::TAB_SIZE;
use crate::rules::IndentUnit;
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
    /// The Visual selection as drawn (bytes, one range per line of a block).
    visual: Vec<Range<usize>>,
    message: Option<VimMessage>,
    /// `'tabstop'`, `'shiftwidth'` and `'expandtab'` follow the file until a `:set` of one.
    indent_from_file: bool,
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
/// produce (a Dvorak layout types its own letters as commands). A character that is not ASCII
/// (a Russian or Greek layout) is matched by the key's place on the keyboard instead, the way
/// macOS names the key (`d` for the key that types `в`), unless `takes_text` says the
/// character itself is wanted there (Insert mode, the search line, the character after `f` or
/// `r`: [`hutan_vim::Vim::takes_text`]).
pub fn vim_key_of(event: &KeyDownEvent, takes_text: impl FnOnce(char) -> bool) -> Option<Key> {
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
    let c = match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => c,
        _ => return None,
    };
    // ⌥ types its own characters (`å`); those are not a key's place.
    if !c.is_ascii()
        && !mods.alt
        && let Some(place) = key_place(&ks.key, mods.shift)
        && !takes_text(c)
    {
        return Some(Key::Char(place));
    }
    Some(Key::Char(c))
}

/// The ASCII character macOS names a printable key by (its character on the layout ⌘ uses, a
/// US one for a Cyrillic layout); a letter with ⇧ is the capital. `None` when the name is not a
/// single ASCII character.
fn key_place(key: &str, shift: bool) -> Option<char> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => {
            Some(if shift { c.to_ascii_uppercase() } else { c })
        }
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

/// The `:g` commands the view runs on a background thread (the rest run through the Vim state).
fn global_in_background(command: &Ex) -> bool {
    matches!(
        command,
        Ex::Delete {
            lines: None,
            register: None,
            count: None,
        } | Ex::Substitute { lines: None, .. }
            | Ex::Normal { lines: None, .. }
    )
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
                        command_line: true,
                    },
                    group_open: false,
                    visual: Vec::new(),
                    message: None,
                    indent_from_file: true,
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

    /// What the `editor.mode` status item shows beside the mode: `recording @a` while a macro
    /// is recorded, and the keys of a command in progress (`2d`). The `/` / `?` line being
    /// typed is not among them: it shows under the code ([`vim_command_line`]).
    ///
    /// [`vim_command_line`]: Self::vim_command_line
    pub fn vim_pending(&self) -> String {
        let Some(state) = &self.vim else {
            return String::new();
        };
        let pending = if state.vim.command_line().is_some() {
            String::new()
        } else {
            state.vim.pending_keys()
        };
        match state.vim.recording() {
            Some(name) if pending.is_empty() => format!("recording @{name}"),
            Some(name) => format!("recording @{name} {pending}"),
            None => pending,
        }
    }

    /// The `/` or `?` line being typed, with its leading character (`/foo`), for the owner to
    /// show under the code as Vim does; Escape or Backspace on an empty line ends it.
    pub fn vim_command_line(&self) -> Option<String> {
        self.vim.as_ref().and_then(|v| v.vim.command_line())
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

    /// The selection to draw: the Visual selection in a Visual mode, else the carets'
    /// selections.
    pub(crate) fn drawn_selections(&self) -> Vec<Range<usize>> {
        match self.vim.as_ref().filter(|v| !v.visual.is_empty()) {
            Some(state) => state.visual.clone(),
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

    /// Where a click at byte `at` puts the caret: in Normal mode on a character, so a click past
    /// the end of a line lands on its last character, as in Vim.
    pub(crate) fn vim_click_target(&self, text: &Text, at: usize) -> usize {
        if self.vim_mode() != Some(Mode::Normal) {
            return at;
        }
        let Ok(content) = text.line_content_range(line_of(text, at)) else {
            return at;
        };
        if at < content.end || content.is_empty() {
            return at;
        }
        text.char_before(content.end)
            .map_or(at, |c| content.end - c.len_utf8())
    }

    // ---- keys --------------------------------------------------------------------------------

    /// A key press that no binding took. Returns whether the view used it.
    pub(crate) fn vim_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(mode) = self.vim_mode() else {
            return false;
        };
        let takes_text = |c| self.vim.as_ref().is_some_and(|v| v.vim.takes_text(c));
        let Some(key) = vim_key_of(event, takes_text) else {
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
        let idle = self
            .vim
            .as_ref()
            .is_some_and(|v| v.vim.pending_keys().is_empty());
        if mode != Mode::Insert && key == Key::Char(':') && idle {
            self.open_command_line(cx);
            return true;
        }
        self.vim_key(key, cx) == Outcome::Handled
    }

    /// `:` in Normal or Visual: Visual ends (its lines become `'<` and `'>`) and the owner opens
    /// the command line.
    fn open_command_line(&mut self, cx: &mut Context<Self>) {
        let visual = self.vim_mode().is_some_and(Mode::is_visual);
        if visual {
            self.vim_key(Key::Escape, cx);
        }
        if let Some(state) = &mut self.vim {
            state.message = None;
        }
        cx.emit(CodeViewEvent::CommandLine { visual });
        cx.notify();
    }

    /// Runs `f` on the Vim state with the text model lent to a [`BufferHost`], and takes the
    /// model back with what `f` did (the selection, the message and the caret follow). `None`
    /// while Vim is off.
    fn with_vim<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut VimState, &mut BufferHost, &mut dyn Clipboard) -> R,
    ) -> Option<R> {
        self.vim.as_ref()?;
        let editable = self.is_editable(cx);
        let id = cx.entity_id();
        let head = self.selections.primary().head;
        let unit = self.buffer().read(cx).indent_unit();
        let model = self.buffer().update(cx, |b, _| b.lend());
        let state = self.vim.as_mut()?;
        if state.indent_from_file {
            let settings = state.vim.settings_mut();
            settings.tab_stop = TAB_SIZE;
            match unit {
                IndentUnit::Tab => {
                    settings.expand_tab = false;
                    settings.shift_width = TAB_SIZE;
                }
                IndentUnit::Spaces(n) => {
                    settings.expand_tab = true;
                    settings.shift_width = n.max(1);
                }
            }
        }
        let mut host = BufferHost::new(model, head, editable, state.group_open);
        let result = {
            let mut clip = AppClipboard(cx);
            f(state, &mut host, &mut clip)
        };
        state.visual = state.vim.selections(&host);
        if let Some(message) = state.vim.take_message() {
            state.message = Some(VimMessage {
                text: message.text,
                error: message.error,
            });
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
        Some(result)
    }

    /// Applies one key to Vim (Normal, Visual, or Escape in Insert): the text model is lent to a
    /// [`BufferHost`] for the key and taken back with what it did.
    pub fn vim_key(&mut self, key: Key, cx: &mut Context<Self>) -> Outcome {
        // Before the file is in, its text is empty and read-only: a key changes only the mode
        // (a new state goes to Normal this way while its file loads).
        let primary = self.selections.primary();
        self.with_vim(cx, |state, host, clip| {
            state.message = None;
            // A selection made with the mouse in Normal mode becomes a Visual one.
            if state.vim.mode() == Mode::Normal && !primary.is_empty() && key != Key::Escape {
                host.set_cursor(primary.anchor);
                state.vim.key(host, Key::Char('v'), state.options, clip);
                let head = if primary.head > primary.anchor {
                    host.text()
                        .char_before(primary.head)
                        .map_or(primary.head, |c| primary.head - c.len_utf8())
                } else {
                    primary.head
                };
                host.set_cursor(head);
            }
            state.vim.key(host, key, state.options, clip)
        })
        .unwrap_or(Outcome::Ignored)
    }

    // ---- the `:` line ------------------------------------------------------------------------

    /// Whether a `:s` or `:g` is still searching (tests wait on it).
    pub fn ex_running(&self) -> bool {
        self.ex_task.is_some()
    }

    /// Parses and runs one `:` line. What concerns the text runs here; the rest is handed back.
    pub fn run_command_line(&mut self, line: &str, cx: &mut Context<Self>) -> ExRun {
        if let Some(state) = &mut self.vim {
            state.message = None;
            state.vim.command_line_ran(line);
        }
        match vim_ex::parse(line) {
            Ok(Some(command)) => self.run_ex(command, cx),
            Ok(None) => ExRun::Done,
            Err(error) => {
                self.set_vim_message(error.0, true, cx);
                ExRun::Done
            }
        }
    }

    /// Runs `command` if it concerns the text; files, windows and the rest are the owner's.
    pub fn run_ex(&mut self, command: Ex, cx: &mut Context<Self>) -> ExRun {
        if command.is_owners() {
            return ExRun::Owner(command);
        }
        let result = match command {
            Ex::Set(options) => self.set_options(options, cx),
            Ex::Substitute { lines, command } => self.substitute(lines, command, cx),
            Ex::Global {
                lines,
                invert,
                pattern,
                command,
            } if global_in_background(&command) => {
                self.global(lines, invert, &pattern, *command, cx)
            }
            other => {
                self.with_vim(cx, |state, host, clip| {
                    state.vim.run(host, other, state.options, clip);
                });
                Ok(())
            }
        };
        if let Err(error) = result {
            self.say(error.0, true);
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        ExRun::Done
    }

    /// `:set`: `number` is the view's gutter; the rest are the Vim state's.
    fn set_options(
        &mut self,
        options: Vec<SetOption>,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let mut answers = Vec::new();
        let mut rest = Vec::new();
        for option in options {
            if option.name != OptionName::Number {
                rest.push(option);
                continue;
            }
            match option.toggle {
                Toggle::On => self.show_numbers = true,
                Toggle::Off => self.show_numbers = false,
                Toggle::Invert => self.show_numbers = !self.show_numbers,
                Toggle::Query => answers.push(if self.show_numbers {
                    "  number".to_owned()
                } else {
                    "nonumber".to_owned()
                }),
                Toggle::Value(_) => {}
            }
        }
        // The gutter may change width: every line is drawn again.
        self.lines.clear();
        let indent = rest.iter().any(|o| {
            matches!(
                o.name,
                OptionName::ShiftWidth | OptionName::TabStop | OptionName::ExpandTab
            ) && o.toggle != Toggle::Query
        });
        if !rest.is_empty() {
            self.with_vim(cx, |state, host, clip| {
                if indent {
                    state.indent_from_file = false;
                }
                state.vim.run(host, Ex::Set(rest), state.options, clip);
            });
        }
        if !answers.is_empty() {
            let vim_answer = self.vim_message().map(|m| m.text.clone());
            answers.extend(vim_answer);
            self.say(answers.join(" "), false);
        }
        Ok(())
    }

    /// `:s`: searched in the background over a snapshot of the rope, applied as one
    /// transaction if the text did not change meanwhile.
    fn substitute(
        &mut self,
        lines: Option<LineSpec>,
        mut command: Substitute,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let prepared = self.with_vim(cx, |state, host, _| -> Result<_, ExError> {
            let mut range =
                state
                    .vim
                    .line_range(host, lines.as_ref(), &LineSpec::One(LineRef::CURRENT))?;
            if let Some(n) = command.count {
                let start = range.end - 1;
                range = start..(start + n).min(host.line_count());
            }
            let pattern = state.vim.prepare_substitute(&mut command)?;
            Ok((range, pattern))
        });
        let (range, pattern) = prepared.ok_or_else(|| ExError("Vim mode is off".into()))??;
        let replacement = command.replacement.clone().unwrap_or_default();
        let (text, version) = self.snapshot(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        let job = {
            let cancel = cancel.clone();
            let global = command.global;
            let count_only = command.count_only;
            cx.background_spawn(async move {
                ex::substitute(
                    &text,
                    range,
                    &pattern,
                    &replacement,
                    global,
                    count_only,
                    &|| cancel.load(Ordering::Relaxed),
                )
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

    /// `:g/pat/cmd` and `:v/pat/cmd` (`invert`) with `d`, `s` or `normal`: the matching lines
    /// are found in the background; `d` and `s` are one transaction, `normal` runs its keys on
    /// every line, in one undo step.
    fn global(
        &mut self,
        lines: Option<LineSpec>,
        invert: bool,
        pattern: &str,
        mut command: Ex,
        cx: &mut Context<Self>,
    ) -> Result<(), ExError> {
        let prepared = self.with_vim(cx, |state, host, _| -> Result<_, ExError> {
            let range = state.vim.line_range(host, lines.as_ref(), &LineSpec::All)?;
            let regex = state.vim.search_pattern(pattern)?;
            let sub = match &mut command {
                Ex::Substitute { command: sub, .. } => {
                    let compiled = state.vim.prepare_substitute(sub)?;
                    Some((compiled, sub.clone()))
                }
                _ => None,
            };
            Ok((range, regex, sub))
        });
        let (range, regex, sub) = prepared.ok_or_else(|| ExError("Vim mode is off".into()))??;
        let pattern = pattern.to_owned();
        let (text, version) = self.snapshot(cx);
        let cancel = Arc::new(AtomicBool::new(false));
        let delete = matches!(command, Ex::Delete { .. });
        let job = {
            let cancel = cancel.clone();
            let sub = sub.clone();
            cx.background_spawn(async move {
                let cancelled = || cancel.load(Ordering::Relaxed);
                let lines = ex::matching_lines(&text, range, &regex, invert, &cancelled);
                let edits = match &sub {
                    Some((compiled, s)) => {
                        let replacement = s.replacement.clone().unwrap_or_default();
                        let mut all = ex::Substitution::default();
                        for &line in &lines {
                            let one = ex::substitute(
                                &text,
                                line..line + 1,
                                compiled,
                                &replacement,
                                s.global,
                                s.count_only,
                                &cancelled,
                            );
                            all.edits.extend(one.edits);
                            all.matches += one.matches;
                            all.lines += one.lines;
                            all.last_line = one.last_line.or(all.last_line);
                        }
                        Some(all)
                    }
                    None if delete => Some(ex::Substitution {
                        edits: ex::delete_lines(&text, &lines),
                        matches: lines.len(),
                        lines: lines.len(),
                        last_line: lines.first().copied(),
                    }),
                    None => None,
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
                match (command, edits, sub) {
                    (Ex::Normal { keys, .. }, _, _) => this.normal_on_lines(&lines, &keys, cx),
                    (Ex::Delete { .. }, Some(found), _) => {
                        let line = found.last_line.unwrap_or(0);
                        if this.apply_lines_edit(found.edits, line, cx) && found.lines > 2 {
                            this.say(format!("{} fewer lines", found.lines), false);
                        }
                    }
                    (Ex::Substitute { .. }, Some(found), Some((_, s))) => {
                        this.finish_substitute(found, version, &s, cx);
                    }
                    _ => {}
                }
            })
            .ok();
        });
        Ok(())
    }

    /// `:g/…/normal keys` on each of `lines` (0-based, in order), as one undo step.
    fn normal_on_lines(&mut self, lines: &[usize], keys: &str, cx: &mut Context<Self>) {
        if !self.buffer().read(cx).is_ready() {
            return;
        }
        self.with_vim(cx, |state, host, clip| {
            state
                .vim
                .normal_on_lines(host, lines, keys, state.options, clip);
        });
    }
}

fn count_message(n: usize, lines: usize, one: &str, many: &str) -> String {
    format!(
        "{n} {} on {lines} line{}",
        if n == 1 { one } else { many },
        if lines == 1 { "" } else { "s" }
    )
}
