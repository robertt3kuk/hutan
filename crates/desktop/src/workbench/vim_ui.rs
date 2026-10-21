//! The GPUI side of Vim mode (`editor.vim_mode`, `docs/design.md` § Vim mode): keystrokes into
//! the modal layer (`hutan-vim`, adapted to `TextInput` in `chat::vim`), the state of text kept as
//! a plain `String`, the mode pill and the caret.
//!
//! Fields that own a [`TextInput`] (the composer, the Tasks form, the attach form) keep a
//! [`Vim`] beside it and call [`apply`]. Fields that keep a `String` (the palette, prompts, the
//! sidebar filter) keep a [`FieldEdit`], which mirrors the string in a `TextInput` for the
//! caret, the undo history and the modal state.

use gpui::{
    AnyElement, App, ClipboardItem, Context, IntoElement, KeyDownEvent, ParentElement, Styled, div,
};
use hutan_settings::SettingsStore;

use super::chat::TextInput;
use super::chat::vim::{Clipboard, Key, Mode, Options, Outcome, Vim};
use super::modal::Modal;
use super::text_field::{EditableText, FieldMut, FieldRef, FieldState, edit_field};
use super::ui;
use super::view::Workbench;
use crate::settings::EditorSettings;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// `editor` settings as the inputs read them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VimConfig {
    pub enabled: bool,
    pub use_system_clipboard: bool,
}

impl VimConfig {
    pub fn of(settings: &SettingsStore) -> Self {
        let editor = settings.get::<EditorSettings>();
        Self {
            enabled: editor.vim_mode,
            use_system_clipboard: editor.vim_use_system_clipboard,
        }
    }

    /// The modal layer's options for a field. A field has no `:` / `/` / `?` line to show.
    pub fn options(self, single_line: bool, insert_passthrough: bool) -> Options {
        Options {
            single_line,
            insert_passthrough,
            use_system_clipboard: self.use_system_clipboard,
            command_line: false,
        }
    }
}

/// The key the modal layer sees for `event`; `None` for ⌘ and fn combinations (shortcuts, not
/// typing) and for keys it has no name for.
pub fn key_of(event: &KeyDownEvent) -> Option<Key> {
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

/// Feeds `event` to the modal layer over `input`. Typed characters in Insert mode are not keys
/// to it: they reach the field through the platform's text input (`text_field`), like every
/// other text.
pub fn apply(
    vim: &mut Vim,
    input: &mut TextInput,
    event: &KeyDownEvent,
    options: Options,
    cx: &mut App,
) -> Outcome {
    match key_of(event) {
        Some(Key::Char(_)) if vim.mode() == Mode::Insert => Outcome::Ignored,
        Some(key) => vim.key(input, key, options, &mut AppClipboard(cx)),
        None => Outcome::Ignored,
    }
}

/// Which way the caret is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    /// A thin bar between two characters (Insert).
    Bar,
    /// A block over the character under the caret (Normal, Visual).
    Block,
}

impl CaretShape {
    pub fn of(mode: Mode) -> Self {
        if mode.block_caret() {
            CaretShape::Block
        } else {
            CaretShape::Bar
        }
    }
}

/// The `NORMAL` / `INSERT` / `VISUAL` pill.
pub fn mode_pill(mode: Mode, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    ui::pill(mode.label(), theme, tokens)
}

/// The state of one plain-`String` field: the text as a [`TextInput`] (caret, selection, the text
/// an input method is composing, undo history) and, under Vim mode, the modal layer. The string
/// stays the field's value; this follows it.
#[derive(Debug, Clone, Default)]
pub struct FieldEdit {
    input: TextInput,
    vim: Vim,
}

impl FieldEdit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// A field that opens for typing: Insert mode, the history forgotten (the register stays).
    pub fn reset(&mut self) {
        self.vim.reset();
        self.input = TextInput::new();
    }

    /// A field that opens with its text selected (typing replaces it): a rename.
    pub fn reset_selected(&mut self, text: &str) {
        self.reset();
        self.input.reset_to(text);
        self.input.select_all();
    }

    /// Selects all of `text`: typing replaces it.
    pub fn select_all(&mut self, text: &str) {
        self.sync(text);
        self.input.select_all();
    }

    /// Whether part of `text` is selected.
    pub fn selected(&self, text: &str) -> bool {
        self.input.text() == text && self.input.selection().is_some()
    }

    /// Follows `text` when it changed behind this field's back (set by code, not typed).
    fn sync(&mut self, text: &str) {
        if self.input.text() != text {
            self.input.reset_to(text);
        }
    }

    /// Runs `f` over the field with `text` mirrored in it; `text` follows what `f` did.
    pub fn with(
        &mut self,
        text: &mut String,
        vim_on: bool,
        multiline: bool,
        f: &mut dyn FnMut(&mut FieldMut<'_>),
    ) {
        self.sync(text);
        f(&mut FieldMut {
            input: &mut self.input,
            vim: vim_on.then_some(&mut self.vim),
            multiline,
        });
        if self.input.text() != text.as_str() {
            *text = self.input.text().to_owned();
        }
    }

    /// What the field's element draws for `text`: its caret and selection while it follows
    /// `text`, else (`text` was set by code) a caret at the end.
    pub fn state(&self, text: &str, vim_on: bool) -> FieldState {
        let vim = vim_on.then_some(&self.vim);
        if self.input.text() == text {
            FieldState::of(&self.input, vim)
        } else {
            FieldState::plain(text, vim)
        }
    }

    /// Applies `event` to `text`. [`Outcome::Ignored`] means the field's own key handling
    /// applies (Enter, Tab, Escape in Normal mode, ↑ ↓ in a single line).
    pub fn key(
        &mut self,
        text: &mut String,
        event: &KeyDownEvent,
        config: VimConfig,
        single_line: bool,
        cx: &mut App,
    ) -> Outcome {
        let mut used = false;
        self.with(text, config.enabled, !single_line, &mut |field| {
            used = edit_field(field, config, event, cx);
        });
        if used {
            Outcome::Handled
        } else {
            Outcome::Ignored
        }
    }

    /// Escape for the fields that handle it as an action (palette, prompts): `true` when the
    /// modal layer used it (Insert or Visual to Normal, a pending command dropped), `false`
    /// when the field's own Escape applies (close it).
    pub fn escape(&mut self, text: &str) -> bool {
        self.sync(text);
        self.vim
            .key(&mut self.input, Key::Escape, Options::default(), &mut ())
            == Outcome::Handled
    }
}

/// A single-line field of a panel or overlay as its parts: the text (growing to fill the
/// field, caret and selection drawn while `focused`, editable by the mouse and the platform's
/// text input through `field`), and the mode pill under Vim mode.
#[allow(clippy::too_many_arguments)] // the field's state and its look, one call per field
pub fn string_field(
    id: impl Into<gpui::ElementId>,
    edit: &FieldEdit,
    text: &str,
    vim_on: bool,
    placeholder: &str,
    focused: bool,
    masked: bool,
    field: FieldRef,
    theme: &Theme,
    tokens: &Tokens,
) -> Vec<AnyElement> {
    let element = EditableText::new(id, edit.state(text, vim_on), theme)
        .placeholder(placeholder.to_owned())
        .focused(focused)
        .masked(masked)
        .field(Some(field));
    let mut parts = vec![div().flex_1().min_w_0().child(element).into_any_element()];
    if vim_on && focused {
        parts.push(mode_pill(edit.mode(), theme, tokens).into_any_element());
    }
    parts
}

// ---- the workbench's overlays ----------------------------------------------------------------

/// The Vim state of every text field the workbench itself owns (its overlays); only one of
/// them has the keyboard at a time.
#[derive(Debug, Default)]
pub struct OverlayEdits {
    pub palette: FieldEdit,
    pub picker: FieldEdit,
    pub agent: FieldEdit,
    pub modal: FieldEdit,
    pub sheet_query: FieldEdit,
    pub sheet_value: FieldEdit,
}

/// A text field of one of the workbench's overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayField {
    /// The command palette's filter.
    Palette,
    /// The ⌘O / ⌘J / worktree picker's filter.
    Picker,
    /// The model filter of the switch-model dialog.
    Agent,
    /// The input of a prompt dialog (a secret's is masked and has no Vim).
    Modal,
    /// The settings sheet's search.
    SheetQuery,
    /// The value being edited in the settings sheet.
    SheetValue,
}

impl Workbench {
    pub(crate) fn vim_config(&self) -> VimConfig {
        VimConfig::of(&self.settings)
    }

    /// The Vim mode of the open palette's filter; `None` when it is closed or Vim mode is off.
    pub fn palette_vim_mode(&self) -> Option<Mode> {
        (self.palette.is_some() && self.vim_config().enabled).then(|| self.edits.palette.mode())
    }

    /// ⌘K "Toggle Vim Mode": flips `editor.vim_mode` in the user's settings (live).
    pub(crate) fn toggle_vim_mode(&mut self, cx: &mut Context<Self>) {
        let on = !self.vim_config().enabled;
        match self
            .settings
            .write_user("editor.vim_mode", toml::Value::Boolean(on))
        {
            Ok(()) => self.set_notice(if on { "Vim mode on" } else { "Vim mode off" }, cx),
            Err(err) => {
                self.set_notice(format!("Cannot save editor.vim_mode: {err}"), cx);
            }
        }
        cx.notify();
    }

    /// The edit state and text of an overlay's field (`None` while it is not there) and whether
    /// Vim applies to it.
    fn overlay_text(&mut self, which: OverlayField) -> Option<(&mut FieldEdit, &mut String, bool)> {
        let edits = &mut self.edits;
        match which {
            OverlayField::Palette => {
                Some((&mut edits.palette, &mut self.palette.as_mut()?.query, true))
            }
            OverlayField::Picker => {
                Some((&mut edits.picker, &mut self.picker.as_mut()?.query, true))
            }
            OverlayField::Agent => Some((
                &mut edits.agent,
                &mut self.agent_picker.as_mut()?.model_query,
                true,
            )),
            OverlayField::Modal => {
                let modal = self.modal.as_mut()?;
                let vim = !modal.secure_input;
                Some((&mut edits.modal, modal.input.as_mut()?, vim))
            }
            OverlayField::SheetQuery => {
                let sheet = self.sheet.as_mut()?;
                if sheet.edit.is_some() {
                    return None;
                }
                Some((&mut edits.sheet_query, &mut sheet.query, true))
            }
            OverlayField::SheetValue => {
                let edit = self.sheet.as_mut()?.edit.as_mut()?;
                Some((&mut edits.sheet_value, &mut edit.text, true))
            }
        }
    }

    /// [`overlay_text`](Self::overlay_text) for reading.
    fn overlay_view(&self, which: OverlayField) -> Option<(&FieldEdit, &str, bool)> {
        let edits = &self.edits;
        match which {
            OverlayField::Palette => Some((&edits.palette, &self.palette.as_ref()?.query, true)),
            OverlayField::Picker => Some((&edits.picker, &self.picker.as_ref()?.query, true)),
            OverlayField::Agent => {
                Some((&edits.agent, &self.agent_picker.as_ref()?.model_query, true))
            }
            OverlayField::Modal => {
                let modal = self.modal.as_ref()?;
                Some((&edits.modal, modal.input.as_deref()?, !modal.secure_input))
            }
            OverlayField::SheetQuery => {
                let sheet = self.sheet.as_ref()?;
                if sheet.edit.is_some() {
                    return None;
                }
                Some((&edits.sheet_query, &sheet.query, true))
            }
            OverlayField::SheetValue => {
                let edit = self.sheet.as_ref()?.edit.as_ref()?;
                Some((&edits.sheet_value, &edit.text, true))
            }
        }
    }

    fn overlay_focus(&self, which: OverlayField) -> gpui::FocusHandle {
        match which {
            OverlayField::Palette => self.palette_focus.clone(),
            OverlayField::Picker => self.picker_focus.clone(),
            OverlayField::Agent => self.agent_focus.clone(),
            OverlayField::Modal => self.modal_focus.clone(),
            OverlayField::SheetQuery | OverlayField::SheetValue => self.sheet_focus.clone(),
        }
    }

    /// What an edit of an overlay's text changes elsewhere: the lists it filters, an error it
    /// clears.
    fn overlay_changed(&mut self, which: OverlayField, cx: &mut Context<Self>) {
        match which {
            OverlayField::Palette => {
                if let Some(query) = self.palette.as_ref().map(|p| p.query.clone()) {
                    self.set_palette_query(&query, cx);
                }
            }
            OverlayField::Picker => {
                if let Some(query) = self.picker.as_ref().map(|p| p.query.clone()) {
                    self.set_picker_query(&query, cx);
                }
            }
            OverlayField::Agent => {
                if let Some(picker) = &mut self.agent_picker {
                    picker.selected = 0;
                }
            }
            OverlayField::SheetQuery => {
                if let Some(query) = self.sheet.as_ref().map(|s| s.query.clone()) {
                    self.set_settings_query(&query, cx);
                }
            }
            OverlayField::SheetValue => {
                if let Some(edit) = self.sheet.as_mut().and_then(|s| s.edit.as_mut()) {
                    edit.error = None;
                }
            }
            OverlayField::Modal => {}
        }
    }

    /// Runs `f` over an overlay's field, then [`overlay_changed`](Self::overlay_changed) when its
    /// text changed.
    fn with_overlay(
        &mut self,
        which: OverlayField,
        f: &mut dyn FnMut(&mut FieldMut<'_>),
        cx: &mut Context<Self>,
    ) {
        let vim_on = self.vim_config().enabled;
        let Some((edit, text, vim)) = self.overlay_text(which) else {
            return;
        };
        let before = text.clone();
        edit.with(text, vim_on && vim, false, f);
        if *text != before {
            self.overlay_changed(which, cx);
        }
    }

    /// How the element of an overlay's field reaches its text.
    pub(crate) fn overlay_field(&self, which: OverlayField, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.overlay_focus(which),
            move |workbench, cx, f| workbench.with_overlay(which, f, cx),
        )
    }

    /// The parts of an overlay's single-line field: the text (editable, caret drawn) and the mode
    /// pill under Vim mode.
    pub(crate) fn overlay_parts(
        &self,
        which: OverlayField,
        placeholder: &str,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some((edit, text, vim)) = self.overlay_view(which) else {
            return Vec::new();
        };
        let tokens = self.density.ui();
        let masked = which == OverlayField::Modal && !vim;
        string_field(
            format!("overlay-field-{which:?}"),
            edit,
            text,
            self.vim_config().enabled && vim,
            placeholder,
            true,
            masked,
            self.overlay_field(which, cx),
            &self.theme,
            &tokens,
        )
    }

    /// A key in an overlay's field: Vim's commands under Vim mode, then the editing keys. `true`
    /// when it was used (Enter, Tab and the arrows of a list are not).
    pub(crate) fn overlay_key(
        &mut self,
        which: OverlayField,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let config = self.vim_config();
        let Some((edit, text, vim)) = self.overlay_text(which) else {
            return false;
        };
        let config = VimConfig {
            enabled: config.enabled && vim,
            ..config
        };
        let before = text.clone();
        let handled = edit.key(text, event, config, true, cx) == Outcome::Handled;
        if *text != before {
            self.overlay_changed(which, cx);
        }
        if handled {
            cx.notify();
        }
        handled
    }

    /// Escape in the palette, the pickers: Insert goes to Normal first. `true` when Vim used it.
    pub(crate) fn overlay_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.vim_config().enabled {
            return false;
        }
        let used = if let Some(palette) = &self.palette {
            self.edits.palette.escape(&palette.query)
        } else if let Some(picker) = &self.picker {
            self.edits.picker.escape(&picker.query)
        } else if let Some(picker) = &self.agent_picker {
            self.edits.agent.escape(&picker.model_query)
        } else {
            false
        };
        if used {
            cx.notify();
        }
        used
    }

    /// Escape in a prompt dialog.
    pub(crate) fn modal_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.vim_config().enabled {
            return false;
        }
        let Some(Modal {
            input: Some(input),
            secure_input: false,
            ..
        }) = &self.modal
        else {
            return false;
        };
        let used = self.edits.modal.escape(input);
        if used {
            cx.notify();
        }
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_shape_follows_the_mode() {
        assert_eq!(CaretShape::of(Mode::Insert), CaretShape::Bar);
        assert_eq!(CaretShape::of(Mode::Normal), CaretShape::Block);
        assert_eq!(CaretShape::of(Mode::Visual), CaretShape::Block);
    }

    #[test]
    fn a_field_edit_follows_its_string_and_selects_for_a_rename() {
        let mut edit = FieldEdit::new();
        edit.reset_selected("old name");
        let state = edit.state("old name", false);
        assert_eq!(state.selection, Some(0.."old name".len()));
        // The string changed behind the field's back: a caret at the end, no selection.
        let state = edit.state("changed", false);
        assert_eq!(state.cursor, "changed".len());
        assert_eq!(state.selection, None);
        // Typing through the handler's path replaces the selection and the string follows.
        let mut text = "old name".to_owned();
        edit.with(&mut text, false, false, &mut |field| {
            field.input.insert("ы")
        });
        assert_eq!(text, "ы");
    }
}
