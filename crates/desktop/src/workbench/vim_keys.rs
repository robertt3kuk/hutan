//! Vim's keys before anything else of the window sees them (`editor.vim_mode`): `⌃[` is
//! Escape wherever Vim is on, so every Escape of the application (leaving Insert, closing a
//! dialog in Normal mode) answers it too; and the user's mappings (`editor.vim_mappings`,
//! Vim's `inoremap` / `nnoremap` / `xnoremap`) turn the keys typed in a Vim field into the keys
//! it reads. With Vim off nothing changes.
//!
//! A mapping is matched on the keys as Vim reads them (`hutan_code_view::vim_key_of`: in Normal
//! and Visual a Russian layout's keys by their place). A key that may begin a mapping is held
//! back; the mapping applies when its last key comes, and when another key or
//! `editor.vim_timeout_ms` (Vim's `timeoutlen`) comes first the held keys go on as typed. What
//! a mapping sends is never mapped again (non-recursive). The mode is the focused field's
//! `vim_mode` key context; terminals, the `:` line and the `/` `?` line have none to match.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, KeyContext, Keystroke, Modifiers, Subscription, Task, Window};
use hutan_code_view::{VIM_COMMAND_LINE_CONTEXT, VIM_MODE_KEY, VimKey, vim_key_of_keystroke};
use hutan_settings::SettingsStore;

use super::vim_ui::VimConfig;
use crate::settings::{EditorSettings, VimMappings};

/// `⌃[` (on any layout: a Russian one names the key `х`), not Escape itself.
fn is_ctrl_bracket(keystroke: &Keystroke) -> bool {
    keystroke.modifiers.control
        && keystroke.key != "escape"
        && vim_key_of_keystroke(keystroke, |_| true) == Some(VimKey::Escape)
}

// ---- mappings ----------------------------------------------------------------------------------

/// The mode a mapping is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMode {
    Insert,
    Normal,
    /// The three Visual modes.
    Visual,
}

impl MapMode {
    pub const ALL: [MapMode; 3] = [MapMode::Insert, MapMode::Normal, MapMode::Visual];

    /// The mode of a field by the value of its `vim_mode` key context.
    fn of_context(value: &str) -> Option<Self> {
        match value {
            "insert" => Some(MapMode::Insert),
            "normal" => Some(MapMode::Normal),
            "visual" => Some(MapMode::Visual),
            _ => None,
        }
    }

    /// Its table in `editor.vim_mappings`.
    pub fn key(self) -> &'static str {
        match self {
            MapMode::Insert => "insert",
            MapMode::Normal => "normal",
            MapMode::Visual => "visual",
        }
    }

    /// The mode in words.
    pub fn label(self) -> &'static str {
        match self {
            MapMode::Insert => "Insert",
            MapMode::Normal => "Normal",
            MapMode::Visual => "Visual",
        }
    }

    fn table(self, mappings: &VimMappings) -> &std::collections::BTreeMap<String, String> {
        match self {
            MapMode::Insert => &mappings.insert,
            MapMode::Normal => &mappings.normal,
            MapMode::Visual => &mappings.visual,
        }
    }
}

/// One mapping that can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub mode: MapMode,
    /// The keys typed (never empty).
    pub from: Vec<VimKey>,
    /// The keys Vim reads instead (empty for `<Nop>`).
    pub to: Vec<VimKey>,
    /// Both as written in the settings.
    pub from_text: String,
    pub to_text: String,
}

/// A mapping that cannot be used, for the settings sheet's Problems list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingProblem {
    pub mode: MapMode,
    /// `editor.vim_mappings.insert`.
    pub key_path: String,
    pub message: String,
}

/// A `<…>` key name as Vim writes it (case does not matter): `Some(None)` for `<Nop>`, `None`
/// for a name Hutan does not know.
fn named_key(name: &str) -> Option<Option<VimKey>> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "esc" | "escape" => VimKey::Escape,
        "cr" | "enter" | "return" => VimKey::Enter,
        "tab" => VimKey::Tab,
        "bs" | "backspace" => VimKey::Backspace,
        "del" | "delete" => VimKey::Delete,
        "left" => VimKey::Left,
        "right" => VimKey::Right,
        "up" => VimKey::Up,
        "down" => VimKey::Down,
        "home" => VimKey::Home,
        "end" => VimKey::End,
        "space" => VimKey::Char(' '),
        "lt" => VimKey::Char('<'),
        "bar" => VimKey::Char('|'),
        "bslash" => VimKey::Char('\\'),
        "nop" => return Some(None),
        _ => {
            let rest = lower
                .strip_prefix("c-")
                .or_else(|| lower.strip_prefix("ctrl-"))?;
            let mut chars = rest.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            match c {
                '[' => VimKey::Escape,
                ']' => VimKey::Ctrl(']'),
                '^' | '6' => VimKey::Ctrl('^'),
                c if c.is_ascii_alphabetic() => VimKey::Ctrl(c),
                _ => return None,
            }
        }
    };
    Some(Some(key))
}

/// Keys written as in Vim's mappings: characters as themselves, `<Esc>` `<CR>` `<Tab>` `<BS>`
/// `<Del>` the arrows `<Home>` `<End>` `<Space>` `<lt>` `<Bar>` `<Bslash>` `<C-x>` `<Nop>`. A `<`
/// without a closing `>` is the character.
pub fn parse_keys(text: &str) -> Result<Vec<VimKey>, String> {
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest[1..].find(['>', '<']).map(|at| at + 1)
            && rest[end..].starts_with('>')
            && end > 1
        {
            let name = &rest[1..end];
            match named_key(name) {
                Some(key) => keys.extend(key),
                None => return Err(format!("<{name}> is not a key Hutan knows")),
            }
            rest = &rest[end + 1..];
            continue;
        }
        keys.push(VimKey::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    Ok(keys)
}

/// The mappings of `editor.vim_mappings` that can be used, and what is wrong with the others.
pub fn compile(mappings: &VimMappings) -> (Vec<Mapping>, Vec<MappingProblem>) {
    let mut good = Vec::new();
    let mut problems = Vec::new();
    for mode in MapMode::ALL {
        for (from_text, to_text) in mode.table(mappings) {
            let key_path = format!("editor.vim_mappings.{}", mode.key());
            let problem = |message: String| MappingProblem {
                mode,
                key_path: key_path.clone(),
                message: format!("\"{from_text}\" = \"{to_text}\": {message}"),
            };
            let from = match parse_keys(from_text) {
                Ok(keys) if keys.is_empty() => {
                    problems.push(problem("the keys to map are empty".to_owned()));
                    continue;
                }
                Ok(keys) => keys,
                Err(message) => {
                    problems.push(problem(message));
                    continue;
                }
            };
            let to = match parse_keys(to_text) {
                Ok(keys) => keys,
                Err(message) => {
                    problems.push(problem(message));
                    continue;
                }
            };
            good.push(Mapping {
                mode,
                from,
                to,
                from_text: from_text.clone(),
                to_text: to_text.clone(),
            });
        }
    }
    (good, problems)
}

/// The key press that makes a field read `key`.
fn keystroke_of(key: VimKey) -> Keystroke {
    let named = |name: &str| Keystroke {
        modifiers: Modifiers::none(),
        key: name.into(),
        key_char: None,
    };
    match key {
        VimKey::Char(' ') => Keystroke {
            modifiers: Modifiers::none(),
            key: "space".into(),
            key_char: Some(" ".into()),
        },
        VimKey::Char(c) => Keystroke {
            modifiers: Modifiers {
                shift: c.is_ascii_uppercase(),
                ..Modifiers::none()
            },
            key: c.to_ascii_lowercase().to_string(),
            key_char: Some(c.to_string()),
        },
        VimKey::Ctrl(c) => Keystroke {
            modifiers: Modifiers::control(),
            key: if c == '^' { "6".into() } else { c.to_string() },
            key_char: None,
        },
        VimKey::Escape => named("escape"),
        VimKey::Enter => named("enter"),
        VimKey::Tab => named("tab"),
        VimKey::Backspace => named("backspace"),
        VimKey::Delete => named("delete"),
        VimKey::Left => named("left"),
        VimKey::Right => named("right"),
        VimKey::Up => named("up"),
        VimKey::Down => named("down"),
        VimKey::Home => named("home"),
        VimKey::End => named("end"),
    }
}

/// What the window does with a key press.
#[derive(Debug, PartialEq)]
enum Step {
    /// It goes on as usual.
    Pass,
    /// It stops here; `send` goes instead (keys held back, a mapping's keys), and `wait` holds
    /// the rest back until the next key or the timeout.
    Stop { send: Vec<Keystroke>, wait: bool },
}

/// The window's mapping state.
#[derive(Debug, Default)]
struct Mapper {
    /// The settings the mappings were compiled from.
    source: Option<VimMappings>,
    mappings: Vec<Mapping>,
    /// Keys held back because a mapping may begin with them, as pressed and as Vim reads them.
    held: Vec<(Keystroke, VimKey)>,
    /// The mode the held keys were typed in.
    mode: Option<MapMode>,
    /// Ends the wait for the next key of a mapping.
    timer: Option<Task<()>>,
    /// The mapper's own keys are being sent: they are not mapped again.
    sending: bool,
}

impl Mapper {
    fn refresh(&mut self, mappings: &VimMappings) {
        if self.source.as_ref() != Some(mappings) {
            self.mappings = compile(mappings).0;
            self.source = Some(mappings.clone());
        }
    }

    fn held_keys(&self, upto: usize) -> Vec<VimKey> {
        self.held[..upto].iter().map(|(_, key)| *key).collect()
    }

    /// A mapping of `mode` longer than `seq` begins with it.
    fn longer(&self, mode: MapMode, seq: &[VimKey]) -> bool {
        self.mappings
            .iter()
            .any(|m| m.mode == mode && m.from.len() > seq.len() && m.from.starts_with(seq))
    }

    fn exact(&self, mode: MapMode, seq: &[VimKey]) -> Option<&Mapping> {
        self.mappings
            .iter()
            .find(|m| m.mode == mode && m.from == seq)
    }

    /// Takes the first held keys out: the longest mapping they begin with, else the first key
    /// as typed.
    fn take_front(&mut self, mode: MapMode, send: &mut Vec<Keystroke>) {
        for n in (1..=self.held.len()).rev() {
            let seq = self.held_keys(n);
            if let Some(mapping) = self.exact(mode, &seq) {
                send.extend(mapping.to.iter().copied().map(keystroke_of));
                self.held.drain(..n);
                return;
            }
        }
        let (keystroke, _) = self.held.remove(0);
        send.push(keystroke);
    }

    /// Every held key goes: as the mappings they make up, the rest as typed (the timeout, a key
    /// that is not a Vim field's).
    fn flush(&mut self) -> Vec<Keystroke> {
        let mut send = Vec::new();
        if let Some(mode) = self.mode {
            while !self.held.is_empty() {
                self.take_front(mode, &mut send);
            }
        }
        self.held.clear();
        self.timer = None;
        send
    }

    /// A key pressed in a field in `mode` (`None`: not a Vim field).
    fn key(&mut self, keystroke: &Keystroke, mode: Option<MapMode>) -> Step {
        if self.held.is_empty() && self.mappings.is_empty() {
            return Step::Pass;
        }
        let key = mode.and_then(|m| vim_key_of_keystroke(keystroke, |_| m == MapMode::Insert));
        let (Some(mode), Some(key)) = (mode, key) else {
            if self.held.is_empty() {
                return Step::Pass;
            }
            let mut send = self.flush();
            send.push(keystroke.clone());
            return Step::Stop { send, wait: false };
        };
        let mut send = if self.mode != Some(mode) {
            self.flush()
        } else {
            Vec::new()
        };
        self.mode = Some(mode);
        self.held.push((keystroke.clone(), key));
        loop {
            let seq = self.held_keys(self.held.len());
            if self.longer(mode, &seq) {
                return Step::Stop { send, wait: true };
            }
            if let Some(mapping) = self.exact(mode, &seq) {
                send.extend(mapping.to.iter().copied().map(keystroke_of));
                self.held.clear();
                return Step::Stop { send, wait: false };
            }
            if self.held.len() == 1 {
                // A key no mapping begins with goes on its way untouched.
                if send.is_empty() {
                    self.held.clear();
                    return Step::Pass;
                }
                send.push(self.held.remove(0).0);
                return Step::Stop { send, wait: false };
            }
            // No mapping goes on with these keys: the first ones go, the rest are read again.
            self.take_front(mode, &mut send);
            if self.held.is_empty() {
                return Step::Stop { send, wait: false };
            }
        }
    }
}

/// The mode of the focused Vim field, from the innermost key context that says it.
fn mode_of(context_stack: &[KeyContext]) -> Option<MapMode> {
    if context_stack
        .iter()
        .any(|c| c.contains(VIM_COMMAND_LINE_CONTEXT))
    {
        return None;
    }
    context_stack
        .iter()
        .rev()
        .find_map(|c| c.get(VIM_MODE_KEY))
        .and_then(|value| MapMode::of_context(value))
}

/// Sends `keys` to the focused element as if typed; the mapper does not map them again.
fn send(mapper: &Rc<RefCell<Mapper>>, keys: Vec<Keystroke>, window: &mut Window, cx: &mut App) {
    mapper.borrow_mut().sending = true;
    for keystroke in keys {
        window.dispatch_keystroke(keystroke, cx);
    }
    mapper.borrow_mut().sending = false;
}

/// Installs the window's interceptor.
pub(crate) fn install(settings: SettingsStore, window: &Window, cx: &mut App) -> Subscription {
    let window_handle = window.window_handle();
    let mapper = Rc::new(RefCell::new(Mapper::default()));
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle() != window_handle || mapper.borrow().sending {
            return;
        }
        if !VimConfig::of(&settings).enabled {
            let mut state = mapper.borrow_mut();
            state.held.clear();
            state.timer = None;
            return;
        }
        // A terminal reads `⌃[` itself (it sends ESC, as Escape does), and has no mappings.
        let terminal = event
            .context_stack
            .iter()
            .any(|context| context.contains(hutan_terminal_view::KEY_CONTEXT));
        if !terminal && is_ctrl_bracket(&event.keystroke) {
            cx.stop_propagation();
            let escape = Keystroke {
                modifiers: Modifiers::none(),
                key: "escape".into(),
                key_char: None,
            };
            window.defer(cx, move |window, cx| {
                window.dispatch_keystroke(escape, cx);
            });
            return;
        }
        // A modifier pressed alone (or a key without a name) is no key of a mapping.
        if matches!(
            event.keystroke.key.as_str(),
            "" | "shift" | "control" | "alt" | "platform" | "function"
        ) {
            return;
        }
        let editor = settings.get::<EditorSettings>();
        let mode = if terminal {
            None
        } else {
            mode_of(&event.context_stack)
        };
        let step = {
            let mut state = mapper.borrow_mut();
            state.refresh(&editor.vim_mappings);
            state.key(&event.keystroke, mode)
        };
        let Step::Stop { send: keys, wait } = step else {
            return;
        };
        cx.stop_propagation();
        if !keys.is_empty() {
            let mapper = Rc::clone(&mapper);
            window.defer(cx, move |window, cx| send(&mapper, keys, window, cx));
        }
        let timer = wait.then(|| {
            let mapper = Rc::clone(&mapper);
            let timeout = Duration::from_millis(editor.vim_timeout_ms);
            window.spawn(cx, async move |cx| {
                cx.background_executor().timer(timeout).await;
                cx.update(|window, cx| {
                    let keys = mapper.borrow_mut().flush();
                    send(&mapper, keys, window, cx);
                })
                .ok();
            })
        });
        mapper.borrow_mut().timer = timer;
    })
}

// ---- the built-in keys -------------------------------------------------------------------------

/// One of Vim's own ⌃ keys, as the Settings Keyboard page lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinKey {
    pub keys: &'static str,
    /// Where it applies.
    pub modes: &'static str,
    pub what: &'static str,
}

const fn builtin(keys: &'static str, modes: &'static str, what: &'static str) -> BuiltinKey {
    BuiltinKey { keys, modes, what }
}

/// Vim's control keys Hutan answers (FX-VIM-2), besides the plain keys of `docs/editor.md`
/// § Vim command matrix.
pub const BUILTIN_KEYS: &[BuiltinKey] = &[
    builtin("⌃[", "all modes", "Escape, also on a Russian layout"),
    builtin(
        "⌃C",
        "all modes",
        "leave Insert or Visual, cancel a command or the : / ? line",
    ),
    builtin("⌃W", "Insert", "delete the word before the caret"),
    builtin(
        "⌃U",
        "Insert",
        "delete back to where the insert began, then to the line start",
    ),
    builtin("⌃H", "Insert", "Backspace"),
    builtin("⌃J ⌃M", "Insert", "new line"),
    builtin("⌃T ⌃D", "Insert", "indent / dedent the line"),
    builtin(
        "⌃O",
        "Insert",
        "run one Normal command, then back to Insert",
    ),
    builtin("⌃R {register}", "Insert", "insert the register's text"),
    builtin("⌃A", "Insert", "insert the text of the last insert"),
    builtin("⌃E ⌃Y", "Insert", "copy the character below / above"),
    builtin("⌃N ⌃P", "Insert (file tabs)", "completion"),
    builtin("⌃D ⌃U", "Normal, Visual", "scroll half a page down / up"),
    builtin("⌃F ⌃B", "Normal, Visual", "scroll a page down / up"),
    builtin(
        "⌃E ⌃Y",
        "Normal, Visual",
        "scroll a line down / up, the caret stays",
    ),
    builtin(
        "⌃A ⌃X",
        "Normal",
        "add / subtract 1 (or the count) to the number at or after the caret",
    ),
    builtin(
        "⌃A ⌃X g⌃A g⌃X",
        "Visual",
        "add / subtract on every selected line; g makes a sequence",
    ),
    builtin("⌃R", "Normal", "redo"),
    builtin("⌃O ⌃I", "Normal", "older / newer place in the jump list"),
    builtin("⌃V", "Normal", "Visual block"),
    builtin("⌃^", "Normal (file tabs)", "the file shown before this one"),
    builtin(
        "⌃G",
        "Normal (file tabs)",
        "the file's name, state and position",
    ),
    builtin(
        "⌃] ⌃T",
        "Normal (file tabs)",
        "go to the definition (language server) / back",
    ),
    builtin("⌃L", "Normal", "redraw (nothing to do)"),
    builtin(
        "⌃W s ⌃W v",
        "Normal (file tabs)",
        "split the pane down / right",
    ),
    builtin(
        "⌃W h j k l",
        "Normal (file tabs)",
        "the pane to the left, below, above, right",
    ),
    builtin(
        "⌃W w ⌃W W",
        "Normal (file tabs)",
        "the next / previous pane",
    ),
    builtin("⌃W q ⌃W c", "Normal (file tabs)", "close the tab, as :q"),
    builtin(
        "⌃W o",
        "Normal (file tabs)",
        "zoom the pane (the others stay open)",
    ),
    builtin("⌃W =", "Normal (file tabs)", "make the panes the same size"),
    builtin("⌃W", ": / ? lines", "delete the word before the caret"),
    builtin("⌃U", ": / ? lines", "delete what was typed"),
    builtin("⌃H", ": / ? lines", "Backspace; on an empty line, close it"),
    builtin("⌃R {register}", ": / ? lines", "insert the register's text"),
    builtin(
        "⌃P ⌃N ↑ ↓",
        ": / ? lines",
        "older / newer lines typed before",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(text: &str) -> Keystroke {
        Keystroke::parse(text).expect("a keystroke")
    }

    fn typed(c: char) -> Keystroke {
        keystroke_of(VimKey::Char(c))
    }

    fn mapper(insert: &[(&str, &str)]) -> Mapper {
        let mut mappings = VimMappings::default();
        for (from, to) in insert {
            mappings.insert.insert((*from).to_owned(), (*to).to_owned());
        }
        let mut mapper = Mapper::default();
        mapper.refresh(&mappings);
        mapper
    }

    fn sent(step: Step) -> Vec<String> {
        match step {
            Step::Pass => vec!["pass".to_owned()],
            Step::Stop { send, wait } => {
                let mut keys: Vec<String> = send.iter().map(|k| k.unparse()).collect();
                if wait {
                    keys.push("wait".to_owned());
                }
                keys
            }
        }
    }

    #[test]
    fn ctrl_bracket_is_escape_on_a_latin_and_a_russian_layout() {
        assert!(is_ctrl_bracket(&keystroke("ctrl-[")));
        assert!(is_ctrl_bracket(&Keystroke {
            modifiers: gpui::Modifiers::control(),
            key: "х".into(),
            key_char: None,
        }));
        assert!(!is_ctrl_bracket(&keystroke("escape")));
        assert!(!is_ctrl_bracket(&keystroke("[")));
        assert!(!is_ctrl_bracket(&keystroke("cmd-[")));
        assert!(!is_ctrl_bracket(&keystroke("ctrl-]")));
    }

    #[test]
    fn keys_are_written_as_in_vim() {
        use VimKey::*;
        assert_eq!(parse_keys("jk"), Ok(vec![Char('j'), Char('k')]));
        assert_eq!(parse_keys("<Esc>"), Ok(vec![Escape]));
        assert_eq!(
            parse_keys("<C-w>h<CR><space><lt><Nop>"),
            Ok(vec![Ctrl('w'), Char('h'), Enter, Char(' '), Char('<')])
        );
        assert_eq!(parse_keys("<C-[>"), Ok(vec![Escape]));
        assert_eq!(parse_keys("<C-^>"), Ok(vec![Ctrl('^')]));
        assert_eq!(parse_keys("a<b"), Ok(vec![Char('a'), Char('<'), Char('b')]));
        assert_eq!(parse_keys("<>"), Ok(vec![Char('<'), Char('>')]));
        assert!(parse_keys("<Esk>").unwrap_err().contains("<Esk>"));
        assert!(parse_keys("<C-1>").is_err());
    }

    #[test]
    fn bad_entries_are_problems_and_the_rest_still_map() {
        let mut mappings = VimMappings::default();
        mappings.insert.insert("jk".into(), "<Esc>".into());
        mappings.insert.insert("jj".into(), "<Esk>".into());
        mappings.normal.insert("<Nop>".into(), "x".into());
        let (good, problems) = compile(&mappings);
        assert_eq!(good.len(), 1);
        assert_eq!(good[0].from_text, "jk");
        assert_eq!(problems.len(), 2);
        assert_eq!(problems[0].key_path, "editor.vim_mappings.insert");
        assert!(problems[0].message.contains("\"jj\" = \"<Esk>\""));
        assert!(problems[0].message.contains("<Esk>"));
        assert_eq!(problems[1].key_path, "editor.vim_mappings.normal");
    }

    #[test]
    fn a_sequence_is_held_then_mapped_or_let_go() {
        let insert = Some(MapMode::Insert);
        let mut m = mapper(&[("jk", "<Esc>")]);
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(sent(m.key(&typed('k'), insert)), ["escape"]);
        // `j` then another key: both go as typed.
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(sent(m.key(&typed('x'), insert)), ["j", "x"]);
        // `jj k`: the first `j` goes, the second may begin the mapping again.
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(sent(m.key(&typed('j'), insert)), ["j", "wait"]);
        assert_eq!(sent(m.key(&typed('k'), insert)), ["escape"]);
        // The timeout lets a lone `j` go.
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(
            m.flush().iter().map(|k| k.unparse()).collect::<Vec<_>>(),
            ["j"]
        );
        // Other keys and other modes pass untouched.
        assert_eq!(sent(m.key(&typed('x'), insert)), ["pass"]);
        assert_eq!(sent(m.key(&typed('j'), Some(MapMode::Normal))), ["pass"]);
        assert_eq!(sent(m.key(&typed('j'), None)), ["pass"]);
    }

    #[test]
    fn a_mapping_that_is_the_start_of_another_waits_and_applies_at_the_timeout() {
        let insert = Some(MapMode::Insert);
        let mut m = mapper(&[("j", "J"), ("jk", "<Esc>"), ("<C-l>", "<Del>")]);
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(
            m.flush().iter().map(|k| k.unparse()).collect::<Vec<_>>(),
            ["shift-j"]
        );
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(sent(m.key(&typed('x'), insert)), ["shift-j", "x"]);
        assert_eq!(sent(m.key(&keystroke("ctrl-l"), insert)), ["delete"]);
        // A key of no Vim field lets what was held go first.
        assert_eq!(sent(m.key(&typed('j'), insert)), ["wait"]);
        assert_eq!(sent(m.key(&keystroke("cmd-s"), None)), ["shift-j", "cmd-s"]);
    }
}
