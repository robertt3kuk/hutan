//! A file tab (`ItemRef::File`, slices E-5 and E-6): the code view of one file
//! (`hutan-code-view`), its find and replace bar, the conflict bar when the file changed on disk
//! under unsaved changes (*Reload · Keep Mine · Show Diff*), and what a file says while it loads,
//! when it is too large to open without asking, and when it cannot be shown at all (binary, not
//! UTF-8, gone, outside what the daemon lets the window read). One view per pane; the buffer is
//! shared through the workbench's buffer store (`files.rs`), which saves it (`file_sync.rs`).

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use hutan_chat_view::diff::DiffLine;
use hutan_code_view::{
    CloseFind, CodeStyle, CodeView, CodeViewEvent, Conflict, FailReason, FileBuffer, Find,
    FindNext, FindPrevious, FindQuery, FindReplace, LoadState, Save,
};
use hutan_settings::SettingsStore;

use super::chat::vim::Outcome;
use super::regions::Regions;
use super::style::{TextTooltip, UiStyle};
use super::text_field::FieldRef;
use super::ui::{self, ButtonKind, Mascot};
use super::vim_ui::{self, FieldEdit, VimConfig};
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The key context of a file tab (find in file is bound here, so ⌘F works from the find field
/// too).
pub const KEY_CONTEXT: &str = "FileView";
/// The key context of the find field.
pub const FIND_CONTEXT: &str = "FileFind";

/// What a file tab asks of the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileViewEvent {
    /// Hand the file to its default application.
    OpenWithSystem(PathBuf),
    /// The user confirmed opening a large file (read-only, no syntax).
    OpenLarge(PathBuf),
    /// The selection or the find state changed (the status bar's position).
    Changed,
    /// ⌘S.
    Save(PathBuf),
    /// The conflict bar's *Reload*: take the file as it is on disk.
    Reload(PathBuf),
    /// The conflict bar's *Keep Mine*: write the buffer over the file.
    Overwrite(PathBuf),
    /// The code lost the keyboard (autosave on focus change).
    FocusLost(PathBuf),
}

/// One tab of a file.
pub struct FileView {
    settings: SettingsStore,
    regions: Regions,
    buffer: Entity<FileBuffer>,
    code: Entity<CodeView>,
    find_open: bool,
    find: String,
    find_edit: FieldEdit,
    find_focus: FocusHandle,
    case_sensitive: bool,
    regex: bool,
    replace_open: bool,
    replace: String,
    replace_edit: FieldEdit,
    replace_focus: FocusHandle,
    /// The conflict's diff (the disk's text to the buffer's), while *Show Diff* is on.
    diff: Option<Vec<DiffLine>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for FileView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileView")
            .field("find_open", &self.find_open)
            .field("find", &self.find)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<FileViewEvent> for FileView {}

impl Focusable for FileView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.code.focus_handle(cx)
    }
}

impl FileView {
    pub fn new(
        settings: SettingsStore,
        regions: Regions,
        buffer: Entity<FileBuffer>,
        style: CodeStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let code = cx.new(|cx| CodeView::new(buffer.clone(), style, cx));
        let subscriptions = vec![
            cx.observe(&buffer, |this, buffer, cx| {
                // The conflict is over (reloaded, saved): its diff goes with it.
                if buffer.read(cx).conflict().is_none() {
                    this.diff = None;
                }
                cx.notify();
            }),
            cx.subscribe(&code, |this, _, event: &CodeViewEvent, cx| match event {
                CodeViewEvent::Changed => {
                    cx.emit(FileViewEvent::Changed);
                    cx.notify();
                }
                CodeViewEvent::FocusLost => {
                    let path = this.buffer.read(cx).path().to_owned();
                    cx.emit(FileViewEvent::FocusLost(path));
                }
            }),
        ];
        Self {
            settings,
            regions,
            buffer,
            code,
            find_open: false,
            find: String::new(),
            find_edit: FieldEdit::new(),
            find_focus: cx.focus_handle(),
            case_sensitive: false,
            regex: false,
            replace_open: false,
            replace: String::new(),
            replace_edit: FieldEdit::new(),
            replace_focus: cx.focus_handle(),
            diff: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn code(&self) -> &Entity<CodeView> {
        &self.code
    }

    pub fn buffer(&self) -> &Entity<FileBuffer> {
        &self.buffer
    }

    pub fn find_open(&self) -> bool {
        self.find_open
    }

    pub fn find_text(&self) -> &str {
        &self.find
    }

    /// Restyles the code (theme or font change).
    pub fn set_style(&mut self, style: CodeStyle, cx: &mut Context<Self>) {
        self.code.update(cx, |code, cx| code.set_style(style, cx));
    }

    /// ⌘F: opens the find bar with the selection (one line of it) as the query, and focuses it.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .code
            .read(cx)
            .selected_text(cx)
            .filter(|s| !s.contains('\n') && !s.is_empty());
        if let Some(selected) = selected {
            self.find = selected;
        }
        self.find_open = true;
        self.find_edit.select_all(&self.find);
        window.focus(&self.find_focus, cx);
        self.search(cx);
        cx.notify();
    }

    /// ⌘⌥F: find with the replace field, which gets the keyboard.
    pub fn open_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(window, cx);
        self.replace_open = true;
        self.replace_edit.select_all(&self.replace);
        window.focus(&self.replace_focus, cx);
        cx.notify();
    }

    pub fn replace_open(&self) -> bool {
        self.replace_open
    }

    /// Sets the replacement (tests, the field).
    pub fn set_replacement(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace = text.to_owned();
        self.replace_open = true;
        cx.notify();
    }

    /// *Replace*: the current match, then the next one is current.
    pub fn replace_one(&mut self, cx: &mut Context<Self>) -> bool {
        let replacement = self.replace.clone();
        self.code
            .update(cx, |code, cx| code.replace_current(&replacement, cx))
    }

    /// *Replace All*: every match, as one undo step. Returns how many.
    pub fn replace_all(&mut self, cx: &mut Context<Self>) -> usize {
        let replacement = self.replace.clone();
        let count = self
            .code
            .update(cx, |code, cx| code.replace_all(&replacement, cx));
        cx.notify();
        count
    }

    /// ⌘S: the workbench saves the file.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let path = self.buffer.read(cx).path().to_owned();
        cx.emit(FileViewEvent::Save(path));
    }

    /// The conflict's diff, while it is shown.
    pub fn diff(&self) -> Option<&[DiffLine]> {
        self.diff.as_deref()
    }

    /// *Show Diff* / *Hide Diff*: what changed between the file on disk (`-`) and the buffer
    /// (`+`), as a unified diff.
    pub fn toggle_diff(&mut self, cx: &mut Context<Self>) {
        if self.diff.take().is_some() {
            cx.notify();
            return;
        }
        let buffer = self.buffer.read(cx);
        let Some(Conflict::Changed { disk: Some(disk) }) = buffer.conflict() else {
            return;
        };
        let disk = normalized(disk);
        let mine = buffer.text().to_string();
        let name = buffer.file_name();
        self.diff = Some(conflict_diff(&name, &disk, &mine));
        cx.notify();
    }

    /// Sets the query (tests, the field) and searches.
    pub fn set_find(&mut self, text: &str, cx: &mut Context<Self>) {
        self.find = text.to_owned();
        self.find_open = true;
        self.search(cx);
        cx.notify();
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let query = FindQuery {
            text: self.find.clone(),
            case_sensitive: self.case_sensitive,
            regex: self.regex,
        };
        self.code
            .update(cx, |code, cx| code.set_find_query(query, cx));
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.code.update(cx, |code, cx| code.find_step(forward, cx));
    }

    /// Escape: closes the find bar, drops the highlights, gives the keyboard back to the code.
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find_open {
            return;
        }
        self.find_open = false;
        self.replace_open = false;
        self.code.update(cx, |code, cx| code.clear_find(cx));
        let focus = self.code.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn find_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(cx.weak_entity(), self.find_focus.clone(), |this, cx, f| {
            let vim_on = VimConfig::of(&this.settings).enabled;
            let mut text = this.find.clone();
            this.find_edit.with(&mut text, vim_on, false, f);
            if text != this.find {
                this.find = text;
                this.search(cx);
            }
            cx.notify();
        })
    }

    fn replace_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.replace_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.settings).enabled;
                let mut text = this.replace.clone();
                this.replace_edit.with(&mut text, vim_on, false, f);
                this.replace = text;
                cx.notify();
            },
        )
    }

    fn on_replace_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vim = VimConfig::of(&self.settings);
        let mut text = self.replace.clone();
        if self.replace_edit.key(&mut text, event, vim, true, cx) == Outcome::Handled {
            self.replace = text;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            // Return replaces one, ⌘Return all of them.
            "enter" if ks.modifiers.platform => {
                self.replace_all(cx);
                cx.stop_propagation();
            }
            "enter" => {
                self.replace_one(cx);
                cx.stop_propagation();
            }
            "escape" if !ks.modifiers.platform => {
                self.close_find(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn on_find_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.settings);
        let mut text = self.find.clone();
        if self.find_edit.key(&mut text, event, vim, true, cx) == Outcome::Handled {
            if text != self.find {
                self.find = text;
                self.search(cx);
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "enter" => {
                self.step(!ks.modifiers.shift, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close_find(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn toggle(&mut self, regex: bool, cx: &mut Context<Self>) {
        if regex {
            self.regex = !self.regex;
        } else {
            self.case_sensitive = !self.case_sensitive;
        }
        self.search(cx);
        cx.notify();
    }

    // ---- rendering --------------------------------------------------------------------------

    fn render_find_bar(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.find_focus.is_focused(window);
        let summary = self.code.read(cx).find_state().summary();
        let failed = self.code.read(cx).find_state().result.error.is_some();
        let toggle = |id: &'static str, label: &'static str, tip: &'static str, on: bool| {
            let hover = theme.surface_hover;
            div()
                .id(id)
                .flex_none()
                .px(px(5.0))
                .rounded(px(tokens.radius - 2.0))
                .border_1()
                .border_color(if on { theme.accent } else { theme.border })
                .text_color(if on { theme.accent } else { theme.muted })
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(SharedString::from(tip))).into())
                .child(label)
        };
        let step = |id: &'static str, icon: crate::icons::Icon, tip: &'static str| {
            let hover = theme.surface_hover;
            div()
                .id(id)
                .flex_none()
                .p(px(3.0))
                .rounded(px(tokens.radius - 2.0))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(SharedString::from(tip))).into())
                .child(crate::icons::icon(icon, 14.0, theme.muted))
        };
        div()
            .id("file-find")
            .key_context(FIND_CONTEXT)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .id("file-find-box")
                    .track_focus(&self.find_focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.on_find_key(event, window, cx);
                    }))
                    .flex_1()
                    .min_w(px(120.0))
                    .max_w(px(420.0))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row - 4.0))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(if focused { theme.accent } else { theme.border })
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, window, cx| {
                            cx.defer_in(window, |this, window, cx| {
                                window.focus(&this.find_focus, cx);
                            });
                        }),
                    )
                    .children(vim_ui::string_field(
                        "file-find-text",
                        &self.find_edit,
                        &self.find,
                        VimConfig::of(&self.settings).enabled,
                        "Find in file",
                        focused,
                        false,
                        self.find_field(cx),
                        theme,
                        tokens,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(72.0))
                    .text_color(if failed { theme.danger } else { theme.muted })
                    .child(summary),
            )
            .child(
                toggle("file-find-case", "Aa", "Match case", self.case_sensitive)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(false, cx))),
            )
            .child(
                toggle("file-find-regex", ".*", "Regular expression", self.regex)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(true, cx))),
            )
            .child(
                toggle("file-find-replace", "⇄", "Replace (⌥⌘F)", self.replace_open).on_click(
                    cx.listener(|this, _, window, cx| {
                        if this.replace_open {
                            this.replace_open = false;
                            cx.notify();
                        } else {
                            this.open_replace(window, cx);
                        }
                    }),
                ),
            )
            .child(
                step(
                    "file-find-prev",
                    crate::icons::Icon::ActionCollapse,
                    "Previous match (⇧Return)",
                )
                .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                step(
                    "file-find-next",
                    crate::icons::Icon::ActionExpand,
                    "Next match (Return)",
                )
                .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                step(
                    "file-find-close",
                    crate::icons::Icon::ActionClose,
                    "Close (Escape)",
                )
                .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
            .into_any_element()
    }

    fn render_replace_bar(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.replace_focus.is_focused(window);
        div()
            .id("file-replace")
            .key_context(FIND_CONTEXT)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .id("file-replace-box")
                    .track_focus(&self.replace_focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.on_replace_key(event, window, cx);
                    }))
                    .flex_1()
                    .min_w(px(120.0))
                    .max_w(px(420.0))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row - 4.0))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(if focused { theme.accent } else { theme.border })
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, window, cx| {
                            cx.defer_in(window, |this, window, cx| {
                                window.focus(&this.replace_focus, cx);
                            });
                        }),
                    )
                    .children(vim_ui::string_field(
                        "file-replace-text",
                        &self.replace_edit,
                        &self.replace,
                        VimConfig::of(&self.settings).enabled,
                        "Replace with",
                        focused,
                        false,
                        self.replace_field(cx),
                        theme,
                        tokens,
                    )),
            )
            .child(
                ui::button(
                    "file-replace-one",
                    "Replace",
                    ButtonKind::Secondary,
                    true,
                    theme,
                    tokens,
                )
                .tooltip(|_, cx| {
                    cx.new(|_| TextTooltip("Replace this match (Return)".into()))
                        .into()
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.replace_one(cx);
                })),
            )
            .child(
                ui::button(
                    "file-replace-all",
                    "Replace All",
                    ButtonKind::Secondary,
                    true,
                    theme,
                    tokens,
                )
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip("Replace every match, as one undo step (⌘Return)".into())
                    })
                    .into()
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.replace_all(cx);
                })),
            )
            .into_any_element()
    }

    /// The bar over a file that changed on disk under unsaved changes (or was deleted): never a
    /// dialog, and never an overwrite unless *Keep Mine* says so.
    fn render_conflict_bar(
        &self,
        conflict: &Conflict,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = self.buffer.read(cx).file_name();
        let path = self.buffer.read(cx).path().to_owned();
        let (message, can_diff) = match conflict {
            Conflict::Changed { disk } => (
                format!("{name} changed on disk, and your changes are not saved."),
                disk.is_some(),
            ),
            Conflict::Deleted => (
                format!("{name} was deleted on disk. Your text is still here."),
                false,
            ),
        };
        let mut actions = div().flex().flex_none().gap(px(tokens.spacing));
        if matches!(conflict, Conflict::Changed { .. }) {
            let reload = path.clone();
            actions = actions.child(
                ui::button(
                    "file-conflict-reload",
                    "Reload",
                    ButtonKind::Secondary,
                    true,
                    theme,
                    tokens,
                )
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip("Take the file from disk (⌘Z brings your text back)".into())
                    })
                    .into()
                })
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(FileViewEvent::Reload(reload.clone()));
                })),
            );
        }
        let keep = path;
        actions = actions.child(
            ui::button(
                "file-conflict-keep",
                if matches!(conflict, Conflict::Deleted) {
                    "Keep Mine (Save Again)"
                } else {
                    "Keep Mine (Overwrite)"
                },
                ButtonKind::Secondary,
                true,
                theme,
                tokens,
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(FileViewEvent::Overwrite(keep.clone()));
            })),
        );
        if can_diff {
            actions = actions.child(
                ui::button(
                    "file-conflict-diff",
                    if self.diff.is_some() {
                        "Hide Diff"
                    } else {
                        "Show Diff"
                    },
                    ButtonKind::Ghost,
                    true,
                    theme,
                    tokens,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_diff(cx))),
            );
        }
        div()
            .id("file-conflict")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .border_b_1()
            .border_color(theme.warning)
            .bg(theme.warning.opacity(0.14))
            .text_size(px(tokens.secondary_font_size))
            .child(crate::icons::icon(
                crate::icons::Icon::StateWarning,
                14.0,
                theme.warning,
            ))
            .child(div().flex_1().min_w_0().text_color(theme.fg).child(message))
            .child(actions)
            .into_any_element()
    }

    /// *Show Diff*: the disk's text (−) against yours (+), drawn as chat cards draw diffs.
    fn render_diff(&self, lines: &[DiffLine], cx: &mut Context<Self>) -> AnyElement {
        let look = super::chat::Look::of(cx);
        let border = look.theme.border;
        let shown = lines.len().min(hutan_chat_view::diff::MAX_LINES);
        let more = lines.len() - shown;
        div()
            .id("file-conflict-diff-lines")
            .flex_none()
            .max_h(px(280.0))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(border)
            .font_family(look.mono.clone())
            .text_size(px(look.tokens.secondary_font_size))
            .children(
                lines[..shown]
                    .iter()
                    .map(|line| super::chat::diff_row(line, &look)),
            )
            .when(lines.is_empty(), |s| {
                s.child(
                    div()
                        .p(px(look.tokens.spacing))
                        .child("No difference in the text."),
                )
            })
            .when(more > 0, |s| {
                s.child(
                    div()
                        .p(px(look.tokens.spacing))
                        .text_color(look.theme.muted)
                        .child(format!("{more} more lines")),
                )
            })
            .into_any_element()
    }

    fn render_state(
        &self,
        state: &LoadState,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let buffer = self.buffer.read(cx);
        let name = buffer.file_name();
        let path = buffer.path().to_owned();
        let system_button = |label: &'static str, kind: ButtonKind| {
            let path = path.clone();
            ui::button("file-open-system", label, kind, true, theme, tokens).on_click(cx.listener(
                move |_, _, _, cx| {
                    cx.emit(FileViewEvent::OpenWithSystem(path.clone()));
                },
            ))
        };
        match state {
            LoadState::Loading => ui::empty_state(
                Mascot::Thinking,
                format!("Opening {name}…"),
                None,
                theme,
                tokens,
            )
            .into_any_element(),
            LoadState::NeedsConfirm { size } => {
                let limit = self
                    .settings
                    .get::<crate::settings::EditorSettings>()
                    .large_file_bytes;
                let open_path = path.clone();
                let actions = div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        ui::button(
                            "file-open-large",
                            "Open Read-Only",
                            ButtonKind::Primary,
                            true,
                            theme,
                            tokens,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(FileViewEvent::OpenLarge(open_path.clone()));
                        })),
                    )
                    .child(system_button("Open with System", ButtonKind::Secondary))
                    .into_any_element();
                ui::empty_state(
                    Mascot::Empty,
                    format!(
                        "{name} is {}, more than editor.large_file_bytes ({}). It opens read-only and without syntax highlighting.",
                        human_size(*size),
                        human_size(limit)
                    ),
                    Some(actions),
                    theme,
                    tokens,
                )
                .into_any_element()
            }
            LoadState::Failed { reason, message } => {
                let text = match reason {
                    FailReason::Binary => {
                        format!("{name} is a binary file; Hutan shows text only.")
                    }
                    FailReason::NotUtf8 => {
                        format!("{name} is not UTF-8 text; Hutan does not convert encodings.")
                    }
                    FailReason::TooLarge => {
                        format!(
                            "{name} is larger than the 64 MiB a file may have to be opened here."
                        )
                    }
                    FailReason::NotAFile => format!("{name} is not a file."),
                    FailReason::Missing => format!("{name} no longer exists."),
                    FailReason::Forbidden | FailReason::Other => {
                        format!("Cannot open {name}: {message}")
                    }
                };
                let action = reason.system_can_open().then(|| {
                    system_button("Open with System", ButtonKind::Primary).into_any_element()
                });
                ui::empty_state(Mascot::Error, text, action, theme, tokens).into_any_element()
            }
            LoadState::Ready => div().into_any_element(),
        }
    }
}

/// File text as the buffer holds it: no byte-order mark, `\n` line breaks.
fn normalized(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

/// The unified diff of `old` (the disk) to `new` (the buffer), read the way chat cards and the
/// Changes view read diffs, so it is drawn the same.
pub fn conflict_diff(name: &str, old: &str, new: &str) -> Vec<DiffLine> {
    let patch = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("{name} (on disk)"), &format!("{name} (yours)"))
        .to_string();
    hutan_chat_view::diff::split_patch(&patch)
        .into_iter()
        .next()
        .map(|file| file.lines)
        .unwrap_or_default()
}

/// `24.3 MB`, `16.0 MB`, `812 KB`, `90 bytes` (decimal units, as Finder shows sizes).
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1000.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} bytes")
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let (state, large) = {
            let buffer = self.buffer.read(cx);
            (buffer.state().clone(), buffer.is_large())
        };
        let ready = state == LoadState::Ready;
        if ready && !self.buffer.read(cx).text().is_empty() {
            crate::frames::content_painted();
        }
        let find_bar =
            (ready && self.find_open).then(|| self.render_find_bar(&theme, &tokens, window, cx));
        let replace_bar = (ready && self.find_open && self.replace_open)
            .then(|| self.render_replace_bar(&theme, &tokens, window, cx));
        let conflict = self.buffer.read(cx).conflict().cloned();
        let conflict_bar = conflict
            .as_ref()
            .filter(|_| ready)
            .map(|c| self.render_conflict_bar(c, &theme, &tokens, cx));
        let diff = self.diff.clone().map(|lines| self.render_diff(&lines, cx));
        let body = if ready {
            self.code.clone().into_any_element()
        } else {
            self.render_state(&state, &theme, &tokens, cx)
        };
        div()
            .id("file-view")
            .key_context(KEY_CONTEXT)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &CloseFind, window, cx| this.close_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindReplace, window, cx| {
                this.open_replace(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Save, _, cx| this.save(cx)))
            .children(conflict_bar)
            .children(diff)
            .children(find_bar)
            .children(replace_bar)
            .when(ready && large, |s| {
                s.child(
                    div()
                        .flex_none()
                        .px(px(tokens.spacing * 2.0))
                        .py(px(tokens.spacing * 0.5))
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.sidebar)
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child("Large file: read-only, without syntax highlighting."),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn sizes_read_like_finder() {
        assert_eq!(human_size(90), "90 bytes");
        assert_eq!(human_size(812_000), "812 KB");
        assert_eq!(human_size(16 * 1024 * 1024), "16.8 MB");
        assert_eq!(human_size(24_300_000), "24.3 MB");
    }
}
