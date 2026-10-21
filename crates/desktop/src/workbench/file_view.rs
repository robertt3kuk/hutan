//! A file tab (`ItemRef::File`, slices E-5, E-6, E-8 and E-10): the code view of one file
//! (`hutan-code-view`), its find and replace bar, the conflict bar when the file changed on disk
//! under unsaved changes (*Reload · Keep Mine · Show Diff*), the agent bar when an agent's turn
//! changed the file (its changes, ↑ ↓ between them, *Reject* the one at the caret, *Accept
//! All*, *Follow*), Vim's `:` line and the answers of its commands, and what a file says while
//! it loads, when it is too large to open without asking, and when it cannot be shown at all
//! (binary, not UTF-8, gone, outside what the daemon lets the window read). One view per pane;
//! the buffer is shared through the workbench's buffer store (`files.rs`), which saves it
//! (`file_sync.rs`).

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use hutan_chat_view::diff::DiffLine;
use hutan_code_view::{
    AcceptAgentChanges, CloseFind, CodeStyle, CodeView, CodeViewEvent, Conflict, ExRun, FailReason,
    FileBuffer, Find, FindNext, FindPrevious, FindQuery, FindReplace, LoadState, NextAgentHunk,
    PreviousAgentHunk, RejectAgentHunk, Save, ToggleFollowAgent, VimSettings,
};
use hutan_settings::SettingsStore;
use hutan_vim::ex::Ex;

use super::chat::vim::Outcome;
use super::regions::Regions;
use super::style::{TextTooltip, UiStyle};
use super::text_field::FieldRef;
use super::ui::{self, ButtonKind, FocusRingExt as _, Mascot};
use super::vim_ui::{self, FieldEdit, VimConfig};
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The key context of a file tab (find in file is bound here, so ⌘F works from the find field
/// too).
pub const KEY_CONTEXT: &str = "FileView";
/// The key context of the find field.
pub const FIND_CONTEXT: &str = "FileFind";
/// Commands kept in the `:` line's history (↑ ↓).
const COMMAND_HISTORY: usize = 100;

/// What a file tab asks of the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileViewEvent {
    /// Hand the file to its default application.
    OpenWithSystem(PathBuf),
    /// The user confirmed opening a large file (read-only, no syntax).
    OpenLarge(PathBuf),
    /// ⌘S.
    Save(PathBuf),
    /// The conflict bar's *Reload*: take the file as it is on disk.
    Reload(PathBuf),
    /// The conflict bar's *Keep Mine*: write the buffer over the file.
    Overwrite(PathBuf),
    /// The code lost the keyboard (autosave on focus change).
    FocusLost(PathBuf),
    /// A `:` command about files, windows, buffers or terminals (`:w :q :e :sp :vs :b :ls
    /// :term`), for the workbench to run.
    Ex(Ex),
    /// The agent bar's *Accept All*: keep the agent's changes to the file and stop tracking
    /// them (the pending ledger drops the file).
    AcceptAgentChanges(PathBuf),
}

/// Vim's `:` line while it is open.
#[derive(Debug, Default)]
struct CommandLine {
    text: String,
    edit: FieldEdit,
    /// Where ↑ / ↓ are in the history (`None`: the line being typed).
    at: Option<usize>,
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
    /// Vim's `:` line, while open.
    command: Option<CommandLine>,
    command_focus: FocusHandle,
    /// The `:` commands run, oldest first.
    history: Vec<String>,
    /// The `:` line opened and takes the keyboard at the next frame (the event that opens it
    /// comes without a window).
    focus_command_on_render: bool,
    /// *Follow agent*: the view scrolls to the agent's newest change at every write.
    follow: bool,
    /// The agent's write that Follow scrolled to last.
    followed_epoch: u64,
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
                this.follow_agent(cx);
                cx.notify();
            }),
            cx.subscribe(&code, |this, _, event: &CodeViewEvent, cx| match event {
                CodeViewEvent::Changed => cx.notify(),
                CodeViewEvent::FocusLost => {
                    let path = this.buffer.read(cx).path().to_owned();
                    cx.emit(FileViewEvent::FocusLost(path));
                }
                CodeViewEvent::CommandLine { visual } => this.open_command_line(*visual, cx),
            }),
        ];
        let mut this = Self {
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
            command: None,
            command_focus: cx.focus_handle(),
            history: Vec::new(),
            focus_command_on_render: false,
            follow: false,
            followed_epoch: 0,
            _subscriptions: subscriptions,
        };
        this.sync_vim(cx);
        this
    }

    // ---- an agent's changes (E-10) -----------------------------------------------------------

    /// Whether *Follow agent* is on in this tab.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Turns *Follow agent* on or off; on, the view goes to the agent's newest change now.
    pub fn set_follow(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.follow == on {
            return;
        }
        self.follow = on;
        self.followed_epoch = 0;
        self.follow_agent(cx);
        cx.notify();
    }

    /// While following: once the hunks of the agent's latest write are known, the view shows
    /// the newest one (once per write).
    fn follow_agent(&mut self, cx: &mut Context<Self>) {
        if !self.follow {
            return;
        }
        let (epoch, newest) = {
            let b = self.buffer.read(cx);
            if !b.agent_hunks_current() {
                return;
            }
            (b.agent_epoch(), b.newest_agent_hunk())
        };
        if epoch <= self.followed_epoch {
            return;
        }
        if let Some(index) = newest {
            self.followed_epoch = epoch;
            self.code
                .update(cx, |code, cx| code.reveal_agent_hunk(index, cx));
        }
    }

    /// *Reject* on the agent's change at the caret: the text before the turn goes back in its
    /// place as one undo step; a file without other unsaved changes is saved, so the disk (and
    /// the agent's next read) has it too.
    pub fn reject_agent_hunk(&mut self, cx: &mut Context<Self>) -> bool {
        let clean = !self.buffer.read(cx).is_dirty();
        let rejected = self.code.update(cx, |code, cx| {
            code.agent_hunk_at_caret(cx)
                .is_some_and(|index| code.reject_agent_hunk(index, cx))
        });
        if rejected && clean {
            self.save(cx);
        }
        rejected
    }

    fn accept_agent_changes(&mut self, cx: &mut Context<Self>) {
        let path = self.buffer.read(cx).path().to_owned();
        cx.emit(FileViewEvent::AcceptAgentChanges(path));
    }

    /// The bar over a file an agent's turn changed: who, how many changes, ↑ ↓ between them,
    /// *Reject* (the change at the caret), *Accept All*, *Follow*. The agent wrote the file
    /// itself; the bar says so.
    fn render_agent_bar(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (who, count, at_caret) = {
            let buffer = self.buffer.read(cx);
            let changes = buffer.agent_changes()?;
            let count = buffer.agent_hunks().len();
            if count == 0 {
                return None;
            }
            let who = changes
                .label
                .title
                .clone()
                .unwrap_or_else(|| "An agent".to_owned());
            (
                who,
                count,
                self.code.read(cx).agent_hunk_at_caret(cx).is_some(),
            )
        };
        let message = format!(
            "{who} changed {count} {} in this file, on disk",
            if count == 1 { "place" } else { "places" }
        );
        let icon_button = |id: &'static str, glyph, tip: &'static str| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(tokens.control_height))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_color(theme.muted)
                .hover(|s| s.bg(theme.surface_hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.into())).into())
                .child(crate::icons::glyph(glyph))
        };
        let reject = ui::button(
            "file-agent-reject",
            "Reject",
            ButtonKind::Secondary,
            at_caret,
            theme,
            tokens,
        )
        .tooltip(move |_, cx| {
            cx.new(|_| {
                TextTooltip(if at_caret {
                    "Put back what was there before this change (⌘⌥Z); ⌘Z undoes it".into()
                } else {
                    "Move the caret into a highlighted change (⌘⌥] / ⌘⌥[) to reject it".into()
                })
            })
            .into()
        })
        .when(at_caret, |b| {
            b.on_click(cx.listener(|this, _, _, cx| {
                this.reject_agent_hunk(cx);
            }))
        })
        .child(super::render::probe(
            &self.regions,
            super::view::Region::FileAgent("reject"),
        ));
        let accept = ui::button(
            "file-agent-accept",
            "Accept All",
            ButtonKind::Ghost,
            true,
            theme,
            tokens,
        )
        .tooltip(|_, cx| {
            cx.new(|_| {
                TextTooltip(
                    "Keep the agent's changes to this file and stop highlighting them".into(),
                )
            })
            .into()
        })
        .on_click(cx.listener(|this, _, _, cx| this.accept_agent_changes(cx)))
        .child(super::render::probe(
            &self.regions,
            super::view::Region::FileAgent("accept"),
        ));
        let follow = self.follow;
        let follow_toggle = div()
            .id("file-agent-follow")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(|_, cx| {
                cx.new(|_| {
                    TextTooltip(
                        "Scroll to the agent's newest change whenever it writes this file".into(),
                    )
                })
                .into()
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_follow(!follow, cx)))
            .child(ui::switch("file-agent-follow-switch", follow, true, theme))
            .child(div().text_color(theme.fg).child("Follow"))
            .child(super::render::probe(
                &self.regions,
                super::view::Region::FileAgent("follow"),
            ));
        Some(
            div()
                .id("file-agent")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.sidebar)
                .text_size(px(tokens.secondary_font_size))
                .child(crate::icons::icon(
                    crate::icons::Icon::NavAgents,
                    14.0,
                    theme.accent,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.fg)
                        .child(message),
                )
                .child(
                    icon_button(
                        "file-agent-previous",
                        crate::icons::Icon::ActionCollapse,
                        "Previous change (⌘⌥[)",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.code.update(cx, |c, cx| c.previous_agent_hunk(cx));
                    })),
                )
                .child(
                    icon_button(
                        "file-agent-next",
                        crate::icons::Icon::ActionExpand,
                        "Next change (⌘⌥])",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.code.update(cx, |c, cx| c.next_agent_hunk(cx));
                    })),
                )
                .child(reject)
                .child(accept)
                .child(follow_toggle)
                .into_any_element(),
        )
    }

    /// Follows `editor.vim_mode` and `editor.vim_use_system_clipboard` (live).
    pub fn sync_vim(&mut self, cx: &mut Context<Self>) {
        let config = VimConfig::of(&self.settings);
        let settings = config.enabled.then_some(VimSettings {
            use_system_clipboard: config.use_system_clipboard,
        });
        if settings.is_none() {
            self.command = None;
        }
        self.code.update(cx, |code, cx| code.set_vim(settings, cx));
    }

    // ---- Vim's `:` line ---------------------------------------------------------------------

    /// `:` in Normal or Visual mode: the line opens (with `'<,'>` after a Visual selection)
    /// and takes the keyboard.
    fn open_command_line(&mut self, visual: bool, cx: &mut Context<Self>) {
        let text = if visual { "'<,'>" } else { "" }.to_owned();
        let edit = FieldEdit::new();
        self.command = Some(CommandLine {
            text,
            edit,
            at: None,
        });
        self.focus_command_on_render = true;
        cx.notify();
    }

    /// The text of the `:` line while it is open.
    pub fn command_text(&self) -> Option<&str> {
        self.command.as_ref().map(|c| c.text.as_str())
    }

    /// Closes the `:` line without running it; the code gets the keyboard back.
    fn close_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command = None;
        let focus = self.code.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Runs `line` as if typed on the `:` line (Return): what concerns the text runs in the
    /// code view, `:noh` here, and files, windows, buffers and terminals in the workbench.
    pub fn run_command(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.command = None;
        if !line.trim().is_empty() && self.history.last().map(String::as_str) != Some(line) {
            self.history.push(line.to_owned());
            if self.history.len() > COMMAND_HISTORY {
                self.history.remove(0);
            }
        }
        let focus = self.code.focus_handle(cx);
        window.focus(&focus, cx);
        let ran = self
            .code
            .update(cx, |code, cx| code.run_command_line(line, cx));
        match ran {
            ExRun::Done => {}
            ExRun::Owner(Ex::NoHighlight) => {
                if self.find_open {
                    self.close_find(window, cx);
                } else {
                    self.code.update(cx, |code, cx| code.clear_find(cx));
                }
            }
            ExRun::Owner(command) => cx.emit(FileViewEvent::Ex(command)),
        }
        cx.notify();
    }

    /// Shows `text` on the `:` line (the workbench's answer to a command it ran).
    pub fn say(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.code
            .update(cx, |code, cx| code.set_vim_message(text, error, cx));
    }

    fn command_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.command_focus.clone(),
            |this, cx, f| {
                if let Some(command) = &mut this.command {
                    command.edit.with(&mut command.text, false, false, f);
                }
                cx.notify();
            },
        )
    }

    fn on_command_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let plain = !ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.alt;
        let Some(command) = &mut self.command else {
            return;
        };
        match ks.key.as_str() {
            "enter" if plain => {
                let line = command.text.clone();
                self.run_command(&line, window, cx);
                cx.stop_propagation();
                return;
            }
            "escape" if plain => {
                self.close_command_line(window, cx);
                cx.stop_propagation();
                return;
            }
            // Backspace on an empty line closes it, as in Vim.
            "backspace" if plain && command.text.is_empty() => {
                self.close_command_line(window, cx);
                cx.stop_propagation();
                return;
            }
            "up" | "down" if plain && !self.history.is_empty() => {
                let last = self.history.len() - 1;
                command.at = match (ks.key.as_str(), command.at) {
                    ("up", None) => Some(last),
                    ("up", Some(at)) => Some(at.saturating_sub(1)),
                    ("down", Some(at)) if at < last => Some(at + 1),
                    _ => None,
                };
                command.text = command
                    .at
                    .and_then(|at| self.history.get(at).cloned())
                    .unwrap_or_default();
                command.edit = FieldEdit::new();
                cx.notify();
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let config = VimConfig {
            enabled: false,
            use_system_clipboard: false,
        };
        if command.edit.key(&mut command.text, event, config, true, cx) == Outcome::Handled {
            cx.notify();
            cx.stop_propagation();
        }
    }

    /// The line under the code while Vim is on and there is something to show: the `:` line,
    /// or the answer of the last command.
    fn render_command_bar(
        &mut self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bar = div()
            .id("file-command")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_row))
            .px(px(tokens.spacing * 2.0))
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size));
        if let Some(command) = &self.command {
            let focused = self.command_focus.is_focused(window);
            let field = vim_ui::string_field(
                "file-command-text",
                &command.edit,
                &command.text,
                false,
                "",
                focused,
                false,
                self.command_field(cx),
                theme,
                tokens,
            );
            return Some(
                bar.child(div().text_color(theme.fg).child(":"))
                    .child(
                        div()
                            .id("file-command-box")
                            .track_focus(&self.command_focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                this.on_command_key(event, window, cx);
                            }))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .cursor_text()
                            .children(field),
                    )
                    .into_any_element(),
            );
        }
        let message = self.code.read(cx).vim_message().cloned()?;
        Some(
            bar.text_color(if message.error {
                theme.danger
            } else {
                theme.fg
            })
            .child(div().min_w_0().truncate().child(message.text))
            .into_any_element(),
        )
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
                // An option that is on is the selection's fill, not an accent outline.
                .border_color(if on {
                    gpui::transparent_black()
                } else {
                    theme.border
                })
                .when(on, |s| s.bg(theme.accent_soft))
                .text_color(if on { theme.fg } else { theme.muted })
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
                    .border_color(theme.border)
                    .focus_ring(focused, theme, tokens)
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
                    .border_color(theme.border)
                    .focus_ring(focused, theme, tokens)
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
        let agent_bar = ready
            .then(|| self.render_agent_bar(&theme, &tokens, cx))
            .flatten();
        let diff = self.diff.clone().map(|lines| self.render_diff(&lines, cx));
        let body = if ready {
            self.code.clone().into_any_element()
        } else {
            self.render_state(&state, &theme, &tokens, cx)
        };
        if std::mem::take(&mut self.focus_command_on_render) {
            cx.defer_in(window, |this, window, cx| {
                if this.command.is_some() {
                    window.focus(&this.command_focus, cx);
                }
            });
        }
        let command_bar = ready
            .then(|| self.render_command_bar(&theme, &tokens, window, cx))
            .flatten();
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
            .on_action(cx.listener(|this, _: &RejectAgentHunk, _, cx| {
                this.reject_agent_hunk(cx);
            }))
            .on_action(cx.listener(|this, _: &AcceptAgentChanges, _, cx| {
                this.accept_agent_changes(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFollowAgent, _, cx| {
                let on = !this.follow;
                this.set_follow(on, cx);
            }))
            .on_action(cx.listener(|this, _: &NextAgentHunk, _, cx| {
                this.code.update(cx, |c, cx| c.next_agent_hunk(cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousAgentHunk, _, cx| {
                this.code.update(cx, |c, cx| c.previous_agent_hunk(cx));
            }))
            .children(conflict_bar)
            .children(agent_bar)
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
            .children(command_bar)
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
