//! A file tab (`ItemRef::File`, slice E-5): the code view of one file (`hutan-code-view`), its
//! find bar, and what a file says while it loads, when it is too large to open without asking,
//! and when it cannot be shown at all (binary, not UTF-8, gone, outside what the daemon lets the
//! window read). One view per pane; the buffer is shared through the workbench's buffer store
//! (`files.rs`).

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use hutan_code_view::{
    CloseFind, CodeStyle, CodeView, CodeViewEvent, FailReason, FileBuffer, Find, FindNext,
    FindPrevious, FindQuery, LoadState,
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
            cx.observe(&buffer, |_, _, cx| cx.notify()),
            cx.subscribe(&code, |_, _, event: &CodeViewEvent, cx| match event {
                CodeViewEvent::Changed => {
                    cx.emit(FileViewEvent::Changed);
                    cx.notify();
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
            .children(find_bar)
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
