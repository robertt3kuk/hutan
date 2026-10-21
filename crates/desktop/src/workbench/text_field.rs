//! The text field every Hutan field is made of (brief TI-1): one GPUI element that draws a
//! [`TextInput`] (caret, selection, the underlined text an input method is composing), answers
//! mouse clicks, drags, double and triple clicks, and registers the platform's text input handler
//! while its focus handle has the keyboard.
//!
//! Text reaches a field **only** through that handler (`replace_text_in_range`): plain key
//! presses, input methods (Japanese, Chinese, Korean), dictation, press-and-hold accents, the
//! emoji picker and synthesized text (accessibility tools, automation) all arrive the same way, in
//! UTF-16 ranges the handler converts. `on_key_down` keeps the editing keys, which [`edit_keys`]
//! applies: arrows, Backspace, Return in a multi-line field, ⌘A/C/X/V/Z, ⌥ and ⌘ motions and the
//! Shift variants. A field's owner (the view whose state holds the text) hands the element a
//! [`FieldRef`]: how to reach the [`TextInput`] (and its Vim state) to edit it.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    Element, ElementId, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InputHandler,
    InspectorElementId, IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Style, TextAlign, TextRun,
    TextStyle, UTF16Selection, UnderlineStyle, WeakEntity, Window, WrappedLine, fill, point, px,
    relative, size,
};

use super::chat::TextInput;
use super::chat::vim::{Mode, Outcome, Vim};
use super::vim_ui::{self, VimConfig};
use crate::theme::Theme;

/// What a masked field (a secret) shows for each character.
const MASK: char = '•';

// ---- the owner's side ------------------------------------------------------------------------

/// A field's text and, while `editor.vim_mode` is on, its modal state: what an edit works on.
#[derive(Debug)]
pub struct FieldMut<'a> {
    pub input: &'a mut TextInput,
    /// The field's Vim state under Vim mode.
    pub vim: Option<&'a mut Vim>,
    /// Whether a line break is part of the field's text.
    pub multiline: bool,
}

impl FieldMut<'_> {
    /// Whether typed text goes in: always, except in Vim's Normal and Visual mode, where keys
    /// are commands and a character that slips through the platform's text input is dropped.
    pub fn accepts_text(&self) -> bool {
        self.vim
            .as_ref()
            .is_none_or(|vim| vim.mode() == Mode::Insert)
    }

    /// What the platform's text input may insert: a single-line field takes no line breaks (a
    /// run of them becomes one space), control characters never go in.
    fn cleaned(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' | '\n' => {
                    if c == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    out.push(if self.multiline { '\n' } else { ' ' });
                }
                '\t' => out.push(if self.multiline { '\t' } else { ' ' }),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
        out
    }
}

type WithField = dyn Fn(&mut App, bool, &mut dyn FnMut(&mut FieldMut<'_>));

/// How the element and the platform's input handler reach a field of some owner entity: run a
/// closure over its [`FieldMut`].
#[derive(Clone)]
pub struct FieldRef {
    focus: FocusHandle,
    with: Rc<WithField>,
    /// The layout of the last frame this field was painted in (bounds queries, tests).
    layout: Rc<RefCell<Option<Rc<Layout>>>>,
}

impl FieldRef {
    /// `with(owner, cx, f)` must call `f` with the field's state (or not at all when the field is
    /// gone) and then do what the owner does after the field changed (follow the text into a
    /// mirrored string, refilter a list). `focus` is the handle whose focus gives the field the
    /// platform's text input.
    pub fn new<V: 'static>(
        owner: WeakEntity<V>,
        focus: FocusHandle,
        with: impl Fn(&mut V, &mut Context<V>, &mut dyn FnMut(&mut FieldMut<'_>)) + 'static,
    ) -> Self {
        let with = move |cx: &mut App, notify: bool, f: &mut dyn FnMut(&mut FieldMut<'_>)| {
            owner
                .update(cx, |owner, cx| {
                    with(owner, cx, f);
                    if notify {
                        cx.notify();
                    }
                })
                .ok();
        };
        Self {
            focus,
            with: Rc::new(with),
            layout: Rc::default(),
        }
    }

    /// Reads from the field.
    pub fn read<R>(&self, cx: &mut App, f: impl FnOnce(&mut FieldMut<'_>) -> R) -> Option<R> {
        self.run(cx, false, f)
    }

    /// Edits the field; the owner redraws.
    pub fn edit<R>(&self, cx: &mut App, f: impl FnOnce(&mut FieldMut<'_>) -> R) -> Option<R> {
        self.run(cx, true, f)
    }

    fn run<R>(
        &self,
        cx: &mut App,
        notify: bool,
        f: impl FnOnce(&mut FieldMut<'_>) -> R,
    ) -> Option<R> {
        let mut f = Some(f);
        let mut out = None;
        (self.with)(cx, notify, &mut |field| {
            if let Some(f) = f.take() {
                out = Some(f(field));
            }
        });
        out
    }

    pub(crate) fn layout(&self) -> Option<Rc<Layout>> {
        self.layout.borrow().clone()
    }

    /// The platform's text input handler for this field.
    pub(crate) fn handler(&self) -> FieldHandler {
        FieldHandler(self.clone())
    }
}

// ---- the editing keys ------------------------------------------------------------------------

fn motion(input: &mut TextInput, shift: bool, motion: impl FnOnce(&mut TextInput)) -> bool {
    if shift {
        input.extend(motion);
    } else {
        motion(input);
    }
    true
}

fn copy(cx: &mut App, text: &str) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
}

/// The first line of `text`: what a single-line field takes of a paste.
fn first_line(text: &str) -> &str {
    text.split(['\r', '\n']).next().unwrap_or("")
}

/// Applies an editing key to a field: ⌘A select all, ⌘C copy, ⌘X cut, ⌘V paste (a multi-line
/// field takes all of it, a single-line one its first line), ⌘Z and ⌘⇧Z undo and redo, ⌘⌫, ⌘← ⌘→
/// (line ends), ⌘↑ ⌘↓ (text ends in a multi-line field), ⌥← ⌥→ ⌥⌫ (words), ⌃A ⌃E ⌃B ⌃F ⌃U, ← → ↑ ↓
/// Home End and Backspace Delete, Return in a multi-line field, and the Shift variants of the
/// motions, which select. Text keys are not here: they reach the field through the platform's
/// input handler. `true` when the key was used.
pub fn edit_keys(field: &mut FieldMut<'_>, event: &KeyDownEvent, cx: &mut App) -> bool {
    let ks = &event.keystroke;
    let mods = &ks.modifiers;
    let (multiline, shift) = (field.multiline, mods.shift);
    let input = &mut *field.input;
    let key = ks.key.as_str();
    if mods.function {
        return false;
    }
    if mods.platform {
        if mods.control || mods.alt {
            return false;
        }
        return match key {
            "a" if !shift => {
                input.select_all();
                true
            }
            "c" if !shift => {
                if let Some(text) = input.selected_text() {
                    let text = text.to_owned();
                    copy(cx, &text);
                }
                true
            }
            "x" if !shift => {
                if let Some(text) = input.cut() {
                    copy(cx, &text);
                }
                true
            }
            "v" if !shift => {
                let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                    return false;
                };
                let text = if multiline {
                    text
                } else {
                    first_line(&text).to_owned()
                };
                let text = field.cleaned(&text);
                field.input.insert(&text);
                true
            }
            "z" => {
                if shift {
                    input.redo()
                } else {
                    input.undo()
                }
            }
            "backspace" => {
                input.delete_to_line_start();
                true
            }
            "left" => motion(input, shift, TextInput::home),
            "right" => motion(input, shift, TextInput::end),
            "up" if multiline => motion(input, shift, TextInput::text_start),
            "down" if multiline => motion(input, shift, TextInput::text_end),
            _ => false,
        };
    }
    if mods.control {
        return match key {
            "a" => motion(input, shift, TextInput::home),
            "e" => motion(input, shift, TextInput::end),
            "b" => motion(input, shift, TextInput::left),
            "f" => motion(input, shift, TextInput::right),
            "u" => {
                input.delete_to_line_start();
                true
            }
            _ => false,
        };
    }
    if mods.alt {
        // Any other Option key composes a character (é, ø): the platform's text input's.
        return match key {
            "left" => motion(input, shift, TextInput::word_left),
            "right" => motion(input, shift, TextInput::word_right),
            "backspace" => {
                input.delete_word_back();
                true
            }
            _ => false,
        };
    }
    match key {
        "backspace" => input.backspace(),
        "delete" => input.delete(),
        "left" => return motion(input, shift, TextInput::left),
        "right" => return motion(input, shift, TextInput::right),
        "home" => return motion(input, shift, TextInput::home),
        "end" => return motion(input, shift, TextInput::end),
        "up" if multiline => {
            return motion(input, shift, |input| {
                input.up();
            });
        }
        "down" if multiline => {
            return motion(input, shift, |input| {
                input.down();
            });
        }
        "enter" if multiline => input.insert("\n"),
        _ => return false,
    }
    true
}

/// Applies a key to a field: Vim's commands first under Vim mode (Escape, and every key in Normal
/// and Visual mode), then [`edit_keys`] (not for a key Vim leaves unused in Normal and Visual
/// mode, where only ⌘ shortcuts apply). `true` when the key was used.
pub fn edit_field(
    field: &mut FieldMut<'_>,
    config: VimConfig,
    event: &KeyDownEvent,
    cx: &mut App,
) -> bool {
    if let Some(vim) = field.vim.as_deref_mut() {
        let options = config.options(!field.multiline, true);
        if vim_ui::apply(vim, &mut *field.input, event, options, cx) == Outcome::Handled {
            return true;
        }
        if vim.mode() != Mode::Insert && !event.keystroke.modifiers.platform {
            return false;
        }
    }
    edit_keys(field, event, cx)
}

// ---- the platform's input handler ------------------------------------------------------------

/// The platform's text input handler of a field: UTF-16 ranges over its [`TextInput`].
pub(crate) struct FieldHandler(FieldRef);

impl FieldHandler {
    /// The caret rectangle and the rectangle of `range` (bytes) from the last layout.
    fn rect(&self, real: Range<usize>) -> Option<Bounds<Pixels>> {
        let layout = self.0.layout()?;
        let (start, end) = (layout.position(real.start), layout.position(real.end));
        let width = if (start.y - end.y).abs() < layout.line_height / 2.0 {
            (end.x - start.x).max(px(1.0))
        } else {
            px(1.0)
        };
        Some(Bounds::new(start, size(width, layout.line_height)))
    }
}

impl InputHandler for FieldHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.0.read(cx, |field| {
            let input = &field.input;
            let bytes = input.selection().unwrap_or(input.cursor()..input.cursor());
            UTF16Selection {
                range: input.utf16_range(&bytes),
                reversed: input.selection_reversed(),
            }
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.0
            .read(cx, |field| {
                field
                    .input
                    .marked()
                    .map(|marked| field.input.utf16_range(&marked))
            })
            .flatten()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let (text, adjusted) = self.0.read(cx, |field| {
            let bytes = field.input.byte_range(&range_utf16);
            (
                field.input.text()[bytes.clone()].to_owned(),
                field.input.utf16_range(&bytes),
            )
        })?;
        *adjusted_range = Some(adjusted);
        Some(text)
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.0.edit(cx, |field| {
            if !field.accepts_text() {
                return;
            }
            // Return and Tab are keys (`on_key_down`), not text: a platform that also reports
            // them as text must not insert them a second time.
            if matches!(text, "\n" | "\r" | "\r\n" | "\t") && replacement_range.is_none() {
                return;
            }
            let text = field.cleaned(text);
            let range = replacement_range.map(|range| field.input.byte_range(&range));
            field.input.commit(range, &text);
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.0.edit(cx, |field| {
            if !field.accepts_text() {
                return;
            }
            let text = field.cleaned(new_text);
            let range = range_utf16.map(|range| field.input.byte_range(&range));
            let selected = new_selected_range.map(|range| {
                crate::workbench::chat::utf16_to_byte(&text, range.start)
                    ..crate::workbench::chat::utf16_to_byte(&text, range.end)
            });
            field.input.mark(range, &text, selected);
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.0.edit(cx, |field| field.input.unmark());
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let bytes = self
            .0
            .read(cx, |field| field.input.byte_range(&range_utf16))?;
        self.rect(bytes)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        let at = self.0.layout()?.index_at(point);
        self.0.read(cx, |field| field.input.utf16_offset(at))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.0.edit(cx, |field| {
            let bytes = field.input.byte_range(&range_utf16);
            field.input.select(bytes.start, bytes.end);
        });
    }

    fn text_length_utf16(&mut self, _window: &mut Window, cx: &mut App) -> Option<usize> {
        self.0.read(cx, |field| {
            crate::workbench::chat::utf16_len(field.input.text())
        })
    }

    fn accepts_text_input(&mut self, _window: &mut Window, cx: &mut App) -> bool {
        self.0
            .read(cx, |field| field.accepts_text())
            .unwrap_or(false)
    }

    fn prefers_ime_for_printable_keys(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.accepts_text_input(window, cx)
    }
}

// ---- the element -----------------------------------------------------------------------------

/// Where the parts of a field are, as drawn: what the platform's input handler and the mouse ask.
pub(crate) struct Layout {
    lines: Vec<WrappedLine>,
    line_height: Pixels,
    /// The top left of the first line (the bounds' origin, moved by the horizontal scroll).
    origin: Point<Pixels>,
    /// The length of the field's text in bytes.
    len: usize,
    /// A masked field: the byte offset of each of its characters. (The text itself is never
    /// kept past the frame it is drawn in.)
    mask: Option<Rc<[usize]>>,
}

impl Layout {
    /// Offset in the shown text of byte `real` of the field's text.
    fn to_shown(&self, real: usize) -> usize {
        shown_offset(self.mask.as_deref(), real)
    }

    /// Byte of the field's text of offset `shown` in the shown text.
    fn to_real(&self, shown: usize) -> usize {
        match &self.mask {
            Some(offsets) => offsets
                .get(shown / MASK.len_utf8())
                .copied()
                .unwrap_or(self.len),
            None => shown.min(self.len),
        }
    }

    /// Where byte `real` of the field's text is drawn (the top left of its line box).
    pub(crate) fn position(&self, real: usize) -> Point<Pixels> {
        let shown = self.to_shown(real);
        let mut origin = self.origin;
        let mut start = 0;
        for line in &self.lines {
            let end = start + line.len();
            if shown <= end {
                let within = line
                    .position_for_index(shown - start, self.line_height)
                    .unwrap_or_default();
                return origin + within;
            }
            origin.y += line.size(self.line_height).height;
            start = end + 1;
        }
        origin
    }

    /// The byte of the field's text closest to `at` (window coordinates).
    pub(crate) fn index_at(&self, at: Point<Pixels>) -> usize {
        let mut top = self.origin.y;
        let mut start = 0;
        for (ix, line) in self.lines.iter().enumerate() {
            let height = line.size(self.line_height).height;
            if at.y < top + height || ix + 1 == self.lines.len() {
                let within = point(at.x - self.origin.x, (at.y - top).max(px(0.0)));
                let ix = match line.closest_index_for_position(within, self.line_height) {
                    Ok(ix) | Err(ix) => ix,
                };
                return self.to_real(start + ix.min(line.len()));
            }
            top += height;
            start += line.len() + 1;
        }
        0
    }
}

/// Offset in the shown text of byte `at` of the field's text: the same, or (a masked field) three
/// bytes for each character before it.
fn shown_offset(mask: Option<&[usize]>, at: usize) -> usize {
    match mask {
        Some(offsets) => offsets.partition_point(|start| *start < at) * MASK.len_utf8(),
        None => at,
    }
}

/// What a field's element draws: the text, where the caret and the selection are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldState {
    pub text: SharedString,
    /// Byte offset of the caret.
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
    pub marked: Option<Range<usize>>,
    /// Vim's Normal and Visual mode draw the caret as a block over a character.
    pub block: bool,
}

impl FieldState {
    /// The state of `input`, with Vim's Visual selection and block caret when `vim` is given.
    pub fn of(input: &TextInput, vim: Option<&Vim>) -> Self {
        Self {
            text: input.text().to_owned().into(),
            cursor: input.cursor().min(input.text().len()),
            selection: vim
                .and_then(|vim| vim.selection(input))
                .or_else(|| input.selection()),
            marked: input.marked(),
            block: vim.is_some_and(|vim| vim.mode().block_caret()),
        }
    }

    /// A text with its caret at the end: a field that follows a string and has not been edited.
    pub fn plain(text: &str, vim: Option<&Vim>) -> Self {
        Self {
            text: text.to_owned().into(),
            cursor: text.len(),
            selection: None,
            marked: None,
            block: vim.is_some_and(|vim| vim.mode().block_caret()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Colors {
    text: Hsla,
    placeholder: Hsla,
    caret: Hsla,
    selection: Hsla,
    /// The color of the character under a block caret.
    block_text: Hsla,
}

impl Colors {
    fn of(theme: &Theme) -> Self {
        Self {
            text: theme.fg,
            // `muted`, not `faint`: a placeholder is read (≥ 4.5:1), `faint` is disabled text.
            placeholder: theme.muted,
            caret: theme.accent,
            selection: Hsla {
                a: 0.3,
                ..theme.accent
            },
            block_text: theme.bg,
        }
    }
}

/// A text field: draws a [`FieldState`] and, given a [`FieldRef`], edits through the platform's
/// text input and the mouse. Sizes itself to the width of its parent (a flex child needs
/// `min_w_0`) and, when multi-line, grows with its lines.
pub struct EditableText {
    id: ElementId,
    state: FieldState,
    placeholder: SharedString,
    colors: Colors,
    multiline: bool,
    /// The length of the field's text in bytes (a masked field's is not its shown text's).
    len: usize,
    /// A masked field: the byte offset of each of its characters.
    mask: Option<Rc<[usize]>>,
    focused: bool,
    field: Option<FieldRef>,
}

impl EditableText {
    pub fn new(id: impl Into<ElementId>, state: FieldState, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            len: state.text.len(),
            state,
            placeholder: SharedString::default(),
            colors: Colors::of(theme),
            multiline: false,
            mask: None,
            focused: true,
            field: None,
        }
    }

    /// Shown, dim, while the field is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Wraps at the element's width and keeps line breaks; else one line that scrolls sideways.
    #[must_use]
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    /// Shows a dot for every character (a secret). The text is replaced by the dots here: past
    /// this call the element knows where its characters were, not what they were.
    #[must_use]
    pub fn masked(mut self, mask: bool) -> Self {
        if mask {
            let text = std::mem::take(&mut self.state.text);
            self.mask = Some(text.char_indices().map(|(at, _)| at).collect());
            self.state.text = text.chars().map(|_| MASK).collect::<String>().into();
        }
        self
    }

    /// Whether the caret and the selection are drawn (the field has the keyboard).
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Makes the field editable.
    #[must_use]
    pub fn field(mut self, field: Option<FieldRef>) -> Self {
        self.field = field;
        self
    }

    /// The text to shape and how each part of it is drawn: the placeholder while the field is
    /// empty; else the (masked) text with the selection tinted, the composition underlined and
    /// the character under a block caret inverted. The last value is a block caret that has no
    /// character to sit on (at the end of a line): it is drawn as a box.
    fn shaped(&self, style: &TextStyle) -> (SharedString, Vec<TextRun>, bool) {
        let run = |len: usize, color: Hsla| TextRun {
            len,
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // A masked field's state text is already the dots; the offsets say where the real
        // characters were.
        let shown: SharedString = self.state.text.clone();
        if shown.is_empty() {
            let text = self.placeholder.clone();
            let runs = if text.is_empty() {
                Vec::new()
            } else {
                vec![run(text.len(), self.colors.placeholder)]
            };
            return (text, runs, self.focused && self.state.block);
        }
        let at = |real_at: usize| shown_offset(self.mask.as_deref(), real_at);
        let cursor = at(self.state.cursor.min(self.len));
        let under = if self.focused && self.state.block {
            shown[cursor..].chars().next().filter(|c| *c != '\n')
        } else {
            None
        };
        let block = under.map(|c| cursor..cursor + c.len_utf8());
        let selection = self
            .focused
            .then(|| self.state.selection.clone())
            .flatten()
            .map(|r| at(r.start)..at(r.end));
        let marked = self.state.marked.clone().map(|r| at(r.start)..at(r.end));
        let mut cuts = vec![0, shown.len()];
        for range in [&block, &selection, &marked].into_iter().flatten() {
            cuts.extend([range.start, range.end]);
        }
        cuts.retain(|cut| *cut <= shown.len() && shown.is_char_boundary(*cut));
        cuts.sort_unstable();
        cuts.dedup();
        let inside = |range: &Option<Range<usize>>, at: usize| {
            range.as_ref().is_some_and(|range| range.contains(&at))
        };
        let runs = cuts
            .windows(2)
            .map(|pair| {
                let mut piece = run(pair[1] - pair[0], self.colors.text);
                if inside(&selection, pair[0]) {
                    piece.background_color = Some(self.colors.selection);
                }
                if inside(&marked, pair[0]) {
                    piece.underline = Some(UnderlineStyle {
                        color: Some(self.colors.text),
                        thickness: px(1.0),
                        wavy: false,
                    });
                }
                if inside(&block, pair[0]) {
                    piece.background_color = Some(self.colors.caret);
                    piece.color = self.colors.block_text;
                }
                piece
            })
            .collect();
        let empty_block = self.focused && self.state.block && under.is_none();
        (shown, runs, empty_block)
    }
}

/// What lives across frames: where a one-line field is scrolled to, whether the mouse is
/// dragging a selection in it.
#[derive(Default)]
struct Persisted {
    scroll: Cell<Pixels>,
    dragging: Cell<bool>,
}

pub struct Prepaint {
    layout: Rc<Layout>,
    hitbox: Hitbox,
    persisted: Rc<Persisted>,
    caret: Point<Pixels>,
    block_at_end: bool,
}

fn shape(
    window: &Window,
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    wrap: Option<Pixels>,
) -> Vec<WrappedLine> {
    window
        .text_system()
        .shape_text(text, font_size, runs, wrap, None)
        .map(|lines| lines.into_iter().collect())
        .unwrap_or_default()
}

impl Element for EditableText {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let line_height = style.line_height_in_pixels(window.rem_size());
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.0).into();
        layout_style.min_size.width = px(0.0).into();
        if !self.multiline {
            layout_style.size.height = line_height.into();
            return (window.request_layout(layout_style, [], cx), ());
        }
        // A multi-line field is as tall as its lines wrapped at the width it is given.
        let (text, runs, _) = self.shaped(&style);
        let font_size = style.font_size.to_pixels(window.rem_size());
        let runs: Vec<TextRun> = if runs.is_empty() {
            vec![TextRun {
                len: text.len(),
                font: style.font(),
                color: self.colors.text,
                background_color: None,
                underline: None,
                strikethrough: None,
            }]
        } else {
            runs
        };
        let id =
            window.request_measured_layout(layout_style, move |known, available, window, _| {
                let wrap = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let lines = shape(window, text.clone(), &runs, font_size, wrap);
                let rows: f32 = lines
                    .iter()
                    .map(|line| (line.wrap_boundaries().len() + 1) as f32)
                    .sum();
                size(wrap.unwrap_or_default(), line_height * rows.max(1.0))
            });
        (id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height_in_pixels(window.rem_size());
        let (text, runs, block_at_end) = self.shaped(&style);
        let wrap = self.multiline.then_some(bounds.size.width);
        let lines = shape(window, text, &runs, font_size, wrap);
        let persisted: Rc<Persisted> = match id {
            Some(id) => window.with_element_state(id, |state: Option<Rc<Persisted>>, _| {
                let state = state.unwrap_or_default();
                (state.clone(), state)
            }),
            None => Rc::default(),
        };
        let mut layout = Layout {
            lines,
            line_height,
            origin: bounds.origin,
            len: self.len,
            mask: self.mask.clone(),
        };
        let cursor = self.state.cursor.min(self.len);
        // A one-line field scrolls sideways to keep its caret in view.
        if !self.multiline {
            let caret_x = layout.position(cursor).x - bounds.origin.x;
            let width = bounds.size.width;
            let mut scroll = persisted.scroll.get();
            if caret_x - scroll > width - px(2.0) {
                scroll = caret_x - width + px(2.0);
            }
            if caret_x < scroll {
                scroll = caret_x;
            }
            if self.focused || scroll > caret_x {
                scroll = scroll.max(px(0.0));
            } else {
                scroll = px(0.0);
            }
            persisted.scroll.set(scroll);
            layout.origin.x -= scroll;
        }
        let caret = layout.position(cursor);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        Prepaint {
            layout: Rc::new(layout),
            hitbox,
            persisted,
            caret,
            block_at_end,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = prepaint.layout.clone();
        let line_height = layout.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let mut origin = layout.origin;
            for line in &layout.lines {
                line.paint_background(
                    origin,
                    line_height,
                    TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                )
                .ok();
                line.paint(
                    origin,
                    line_height,
                    TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                )
                .ok();
                origin.y += line.size(line_height).height;
            }
            // A block over a character is made of the character's own run; a bar (not while
            // text is selected) or a box at the end of a line is a quad.
            if prepaint.block_at_end {
                let caret = Bounds::new(prepaint.caret, size(line_height * 0.5, line_height));
                window.paint_quad(fill(caret, self.colors.caret));
            } else if self.focused && !self.state.block && self.state.selection.is_none() {
                let caret = Bounds::new(prepaint.caret, size(px(1.5), line_height));
                window.paint_quad(fill(caret, self.colors.caret));
            }
        });

        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);
        let Some(field) = self.field.clone() else {
            return;
        };
        *field.layout.borrow_mut() = Some(layout.clone());
        if self.focused {
            window.handle_input(&field.focus, field.handler(), cx);
        }

        let hitbox = prepaint.hitbox.clone();
        let persisted = prepaint.persisted.clone();
        window.on_mouse_event({
            let (field, layout, persisted, hitbox) =
                (field.clone(), layout.clone(), persisted.clone(), hitbox);
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let at = layout.index_at(event.position);
                persisted.dragging.set(true);
                field.edit(cx, |field| match event.click_count {
                    1 if event.modifiers.shift => field.input.extend_to(at),
                    1 => field.input.set_cursor(at),
                    2 => field.input.select_word_at(at),
                    _ => field.input.select_line_at(at),
                });
            }
        });
        window.on_mouse_event({
            let (field, layout, persisted) = (field.clone(), layout, persisted.clone());
            move |event: &MouseMoveEvent, phase, _window, cx| {
                if phase != DispatchPhase::Capture
                    || !persisted.dragging.get()
                    || event.pressed_button != Some(MouseButton::Left)
                {
                    return;
                }
                let at = layout.index_at(event.position);
                field.edit(cx, |field| field.input.extend_to(at));
            }
        });
        window.on_mouse_event(move |event: &MouseUpEvent, _, _, _| {
            if event.button == MouseButton::Left {
                persisted.dragging.set(false);
            }
        });
    }
}

impl std::fmt::Debug for FieldRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldRef").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for EditableText {
    // A field's text can be a secret: never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditableText")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Prepaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepaint").finish_non_exhaustive()
    }
}

impl IntoElement for EditableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
