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
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use hutan_chat_view::diff::DiffLine;
use hutan_code_view::{
    AcceptAgentChanges, CloseFind, CodeStyle, CodeView, CodeViewEvent, Conflict, ExRun, FailReason,
    FileBuffer, Find, FindNext, FindPrevious, FindQuery, FindReplace, LoadState, NextAgentHunk,
    NextGitHunk, PreviousAgentHunk, PreviousGitHunk, RejectAgentHunk, RevertGitHunk, Save,
    StageGitHunk, ToggleBlame, ToggleFollowAgent, ToggleGitPeek, ToggleSoftWrap, VimKey,
    VimSettings, VimWindow, WrapMode,
};
use hutan_settings::SettingsStore;
use hutan_vim::ex::Ex;

use super::chat::vim::Outcome;
use super::regions::Regions;
use super::style::{TextTooltip, UiStyle};
use super::text_field::FieldRef;
use super::ui::{self, ButtonKind, FocusRingExt as _, Mascot};
use super::vim_ui::{self, FieldEdit, VimConfig};
use crate::settings::{EditorSettings, LanguagesSettings, SoftWrap};
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
    /// The code view's hover popover opened at byte `offset`: ask the language server (E-12a).
    Hover { offset: usize },
    /// Go to the definition of the symbol at byte `offset` (E-12a).
    Definition { offset: usize },
    /// Go back to where the last jump came from (E-12a).
    GoBack,
    /// Vim's `⌃]` at byte `offset`: the definition, with a way back for `⌃T`.
    TagJump { offset: usize },
    /// Vim's `⌃T`.
    TagBack,
    /// Vim's `⌃^`: the file shown before this one.
    AlternateFile,
    /// Vim's `⌃W` commands for the panes.
    VimWindow(VimWindow),
    /// The tab wants a blame of the file now, or no longer (the gutter column or `editor.
    /// inline_blame` changed): the workbench asks the daemon, or drops what the buffer holds
    /// when no tab of the file wants one (GIT-HISTORY).
    BlameChanged(PathBuf),
    /// *Stage hunk* in the git peek: stage the change at lines `first_line..end_line` of the
    /// saved file (EDITOR-POLISH).
    GitHunkStage {
        path: PathBuf,
        first_line: usize,
        end_line: usize,
    },
    /// The tab wraps its lines now, or no longer (*Toggle Soft Wrap*, settings, the language):
    /// the status bar's `Wrap` follows.
    WrapChanged,
    /// A click on a line's blame: open the history of the file at that commit.
    OpenCommit { path: PathBuf, commit: String },
    /// The code view asks for completions (E-12b; see [`CodeViewEvent::CompletionRequested`]).
    Completion {
        request: u64,
        offset: usize,
        trigger: Option<char>,
        incomplete: bool,
    },
    /// The code view asks for more about a completion item (E-12b).
    CompletionResolve { list: u64, index: usize },
    /// The code view asks for signature help (E-12b).
    SignatureHelp {
        request: u64,
        offset: usize,
        trigger: Option<char>,
        retrigger: bool,
    },
}

/// Vim's `:` line while it is open.
#[derive(Debug, Default)]
struct CommandLine {
    text: String,
    edit: FieldEdit,
    /// Where ↑ / ↓ / ⌃P / ⌃N are in the history (`None`: the line being typed).
    at: Option<usize>,
    /// `⌃R` waits for a register's name.
    register: bool,
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
    /// What the line under the code showed in the last frame (tests).
    shown_bar: Option<String>,
    /// *Follow agent*: the view scrolls to the agent's newest change at every write.
    follow: bool,
    /// The agent's write that Follow scrolled to last.
    followed_epoch: u64,
    /// A bar (agent or conflict) showed in the last frame: when it goes, the text stays where
    /// it is on screen instead of jumping up by the bar's height.
    had_bar: bool,
    /// The line under the code showed in the last frame: when it comes, the code gives it its
    /// row and brings the caret back into view above it, as Vim's command line does.
    had_command_bar: bool,
    /// The window's language servers, for the bar over the code (E-12a, `lsp_bar.rs`).
    lsp: Option<Entity<super::lsp::LspStore>>,
    /// This tab shows the language server's bar: a window shows one bar for a folder or a
    /// server, in the first pane that needs it (`Workbench::sync_lsp_bars`).
    lsp_bar_shown: bool,
    /// *Toggle Blame*: the gutter's column of git blame (GIT-HISTORY).
    blame: bool,
    /// *Toggle Soft Wrap*: what the tab does instead of what settings say.
    wrap_override: Option<WrapMode>,
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
                // The file's language may have come with its text (a shebang).
                this.sync_soft_wrap(cx);
                cx.notify();
            }),
            cx.subscribe(&code, |this, _, event: &CodeViewEvent, cx| match event {
                CodeViewEvent::Changed => cx.notify(),
                CodeViewEvent::BlameClicked { row } => {
                    let commit = this
                        .buffer
                        .read(cx)
                        .blame_line(*row)
                        .and_then(hutan_code_view::BlameLine::commit)
                        .map(|c| c.id.clone());
                    if let Some(commit) = commit {
                        let path = this.buffer.read(cx).path().to_owned();
                        cx.emit(FileViewEvent::OpenCommit { path, commit });
                    }
                }
                CodeViewEvent::GitHunkStage {
                    first_line,
                    end_line,
                    ..
                } => {
                    let path = this.buffer.read(cx).path().to_owned();
                    cx.emit(FileViewEvent::GitHunkStage {
                        path,
                        first_line: *first_line,
                        end_line: *end_line,
                    });
                }
                CodeViewEvent::FocusLost => {
                    let path = this.buffer.read(cx).path().to_owned();
                    cx.emit(FileViewEvent::FocusLost(path));
                }
                CodeViewEvent::CommandLine { visual } => this.open_command_line(*visual, cx),
                CodeViewEvent::HoverRequested { offset } => {
                    cx.emit(FileViewEvent::Hover { offset: *offset });
                }
                CodeViewEvent::GoToDefinition { offset } => {
                    cx.emit(FileViewEvent::Definition { offset: *offset });
                }
                CodeViewEvent::GoBack => cx.emit(FileViewEvent::GoBack),
                CodeViewEvent::TagJump { offset } => {
                    cx.emit(FileViewEvent::TagJump { offset: *offset });
                }
                CodeViewEvent::TagBack => cx.emit(FileViewEvent::TagBack),
                CodeViewEvent::AlternateFile => cx.emit(FileViewEvent::AlternateFile),
                CodeViewEvent::VimWindow(command) => {
                    cx.emit(FileViewEvent::VimWindow(*command));
                }
                CodeViewEvent::CompletionRequested {
                    request,
                    offset,
                    trigger,
                    incomplete,
                } => cx.emit(FileViewEvent::Completion {
                    request: *request,
                    offset: *offset,
                    trigger: *trigger,
                    incomplete: *incomplete,
                }),
                CodeViewEvent::CompletionResolve { list, index } => {
                    cx.emit(FileViewEvent::CompletionResolve {
                        list: *list,
                        index: *index,
                    });
                }
                CodeViewEvent::SignatureHelpRequested {
                    request,
                    offset,
                    trigger,
                    retrigger,
                } => cx.emit(FileViewEvent::SignatureHelp {
                    request: *request,
                    offset: *offset,
                    trigger: *trigger,
                    retrigger: *retrigger,
                }),
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
            shown_bar: None,
            follow: false,
            followed_epoch: 0,
            had_bar: false,
            had_command_bar: false,
            lsp: None,
            lsp_bar_shown: false,
            blame: false,
            wrap_override: None,
            _subscriptions: subscriptions,
        };
        this.sync_vim(cx);
        this.sync_blame_setting(cx);
        this.sync_soft_wrap(cx);
        this
    }

    /// Shows the window's language servers' bar over the code (E-12a); the tab draws again
    /// when they change.
    pub fn set_lsp(&mut self, lsp: Entity<super::lsp::LspStore>, cx: &mut Context<Self>) {
        self._subscriptions
            .push(cx.observe(&lsp, |_, _, cx| cx.notify()));
        self.lsp = Some(lsp);
    }

    /// Whether this tab is the one that shows the language server's bar (the window's one for
    /// that folder or server).
    pub fn set_lsp_bar_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.lsp_bar_shown != shown {
            self.lsp_bar_shown = shown;
            cx.notify();
        }
    }

    /// Whether the bar shows in this tab (tests).
    pub fn lsp_bar_shown(&self) -> bool {
        self.lsp_bar_shown
    }

    /// What the language server's bar says for this file, whichever tab shows it (tests).
    pub fn lsp_bar_text(&self, cx: &App) -> Option<String> {
        let lsp = self.lsp.as_ref()?;
        let path = self.buffer.read(cx).path().display().to_string();
        let bar = lsp.read(cx).bar(&path)?;
        Some(super::lsp_bar::bar_text(&bar))
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

    /// What the bar over a file an agent's turn changed says: the agent's name (its harness,
    /// not the chat's title, which the bar shows beside it) and how many places it changed;
    /// `None` while no bar shows.
    pub fn agent_bar_text(&self, cx: &App) -> Option<String> {
        let buffer = self.buffer.read(cx);
        let changes = buffer.agent_changes()?;
        let count = buffer.agent_hunks().len();
        if count == 0 {
            return None;
        }
        let who = changes.label.agent.as_deref().unwrap_or("An agent");
        Some(format!(
            "{who} changed {count} {} in this file, on disk",
            if count == 1 { "place" } else { "places" }
        ))
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
        let message = self.agent_bar_text(cx)?;
        let session_name = self
            .buffer
            .read(cx)
            .agent_changes()
            .and_then(|changes| changes.label.title.clone())
            .filter(|title| !title.is_empty());
        let at_caret = self.code.read(cx).agent_hunk_at_caret(cx).is_some();
        let step_button = |id: &'static str, glyph, tip: &'static str| {
            ui::IconButton::new(id, glyph, tip, theme, tokens).compact()
        };
        let reject_tip = if at_caret {
            "Put back what was there before this change (⌘⌥Z); ⌘Z undoes it"
        } else {
            "Move the caret into a highlighted change (⌘⌥] / ⌘⌥[) to reject it"
        };
        let reject = ui::Button::new("file-agent-reject", "Reject", theme, tokens)
            .compact()
            .disabled(!at_caret)
            .tooltip(reject_tip)
            .on_click(cx.listener(|this, _, _, cx| {
                this.reject_agent_hunk(cx);
            }))
            .child(super::render::probe(
                &self.regions,
                super::view::Region::FileAgent("reject"),
            ));
        let accept = ui::Button::new("file-agent-accept", "Accept all", theme, tokens)
            .compact()
            .ghost()
            .tooltip("Keep the agent's changes to this file and stop highlighting them")
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
                        .flex_none()
                        .max_w_full()
                        .truncate()
                        .text_color(theme.fg)
                        .child(message),
                )
                // The session's name beside it, quieter; it gives way first.
                .child(
                    div()
                        .id("file-agent-session")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted)
                        .when_some(session_name, |s, name| {
                            let tip = name.clone();
                            s.child(format!("· {name}")).tooltip(move |_, cx| {
                                cx.new(|_| TextTooltip(tip.clone().into())).into()
                            })
                        }),
                )
                .child(
                    step_button(
                        "file-agent-previous",
                        crate::icons::Icon::ActionCollapse,
                        "Previous change (⌘⌥[)",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.code.update(cx, |c, cx| c.previous_agent_hunk(cx));
                    })),
                )
                .child(
                    step_button(
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

    // ---- soft wrap (EDITOR-WRAP) -----------------------------------------------------------

    /// What settings say for this file now: `languages.<language>.soft_wrap`, else
    /// `editor.soft_wrap` (`auto`: on for Markdown and plain text).
    fn configured_wrap(&self, cx: &App) -> WrapMode {
        let language = self.buffer.read(cx).language();
        self.settings.get::<EditorSettings>().wrap_mode(
            &self.settings.get::<LanguagesSettings>(),
            language.map(|l| l.name()),
        )
    }

    /// How this tab wraps (settings, unless *Toggle Soft Wrap* chose for the tab).
    pub fn soft_wrap(&self, cx: &App) -> WrapMode {
        self.code.read(cx).soft_wrap()
    }

    /// Whether the code wraps its lines (the status bar's `Wrap`).
    pub fn wraps(&self, cx: &App) -> bool {
        self.code.read(cx).wraps_text(cx)
    }

    /// Follows `editor.soft_wrap`, `editor.wrap_column`, `languages.*` and the file's language
    /// (live), unless the tab was toggled by hand.
    pub fn sync_soft_wrap(&mut self, cx: &mut Context<Self>) {
        let mode = match self.wrap_override {
            Some(mode) => mode,
            None => self.configured_wrap(cx),
        };
        if self.code.read(cx).soft_wrap() != mode {
            self.code
                .update(cx, |code, cx| code.set_soft_wrap(mode, cx));
            cx.emit(FileViewEvent::WrapChanged);
        }
    }

    /// *Toggle Soft Wrap* (⌥Z): this tab wraps, or does not, whatever settings say. Wrapping
    /// again uses the width settings ask for, else the window's.
    pub fn toggle_soft_wrap(&mut self, cx: &mut Context<Self>) {
        let now = self.code.read(cx).soft_wrap();
        let next = if now == WrapMode::Off {
            let editor = self.settings.get::<EditorSettings>();
            match editor.soft_wrap {
                SoftWrap::Column => WrapMode::Column(editor.wrap_columns()),
                _ => WrapMode::Viewport,
            }
        } else {
            WrapMode::Off
        };
        // Back to what settings say: the tab follows them again.
        self.wrap_override = (next != self.configured_wrap(cx)).then_some(next);
        self.sync_soft_wrap(cx);
        cx.notify();
    }

    // ---- git blame (GIT-HISTORY) -------------------------------------------------------------

    /// Whether the gutter's column of blame is on in this tab.
    pub fn blame_on(&self) -> bool {
        self.blame
    }

    /// Whether the peek of this tab's git hunks offers *Stage hunk*.
    pub fn set_git_stage_available(&mut self, available: bool, cx: &mut Context<Self>) {
        self.code
            .update(cx, |code, cx| code.set_git_stage_available(available, cx));
    }

    /// Whether this tab shows any blame: the column, or the caret line's (`editor.inline_blame`).
    pub fn wants_blame(&self) -> bool {
        self.blame || self.settings.get::<EditorSettings>().inline_blame
    }

    /// *Toggle Blame*: the column of the gutter that says, per line, who changed it last.
    pub fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        self.blame = !self.blame;
        let on = self.blame;
        self.code
            .update(cx, |code, cx| code.set_blame_gutter(on, cx));
        let path = self.buffer.read(cx).path().to_owned();
        cx.emit(FileViewEvent::BlameChanged(path));
        cx.notify();
    }

    /// Follows `editor.inline_blame` (live).
    pub fn sync_blame_setting(&mut self, cx: &mut Context<Self>) {
        let inline = self.settings.get::<EditorSettings>().inline_blame;
        if self.code.read(cx).inline_blame() != inline {
            self.code
                .update(cx, |code, cx| code.set_inline_blame(inline, cx));
            let path = self.buffer.read(cx).path().to_owned();
            cx.emit(FileViewEvent::BlameChanged(path));
        }
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
            register: false,
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
        let vim_key = hutan_code_view::vim_key_of_keystroke(ks, |_| true);
        // `⌃R {reg}`: the register's text goes in at the caret.
        if command.register {
            command.register = false;
            let text = match vim_key {
                Some(VimKey::Char(name)) => self.code.read(cx).vim_register(name, cx),
                _ => None,
            };
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                command
                    .edit
                    .with(&mut command.text, false, false, &mut |f| {
                        f.input.insert(&text)
                    });
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let history_key = match vim_key {
            Some(VimKey::Up | VimKey::Ctrl('p')) => Some(true),
            Some(VimKey::Down | VimKey::Ctrl('n')) => Some(false),
            _ => None,
        };
        match vim_key {
            Some(VimKey::Enter) if plain => {
                let line = command.text.clone();
                self.run_command(&line, window, cx);
                cx.stop_propagation();
                return;
            }
            // Escape, `⌃[` and `⌃C` leave the line without running it.
            Some(VimKey::Escape | VimKey::Ctrl('c')) => {
                self.close_command_line(window, cx);
                cx.stop_propagation();
                return;
            }
            // Backspace or `⌃H` on an empty line closes it, as in Vim.
            Some(VimKey::Backspace | VimKey::Ctrl('h')) if command.text.is_empty() => {
                self.close_command_line(window, cx);
                cx.stop_propagation();
                return;
            }
            Some(key @ VimKey::Ctrl('h' | 'w' | 'u')) => {
                command
                    .edit
                    .with(&mut command.text, false, false, &mut |f| match key {
                        VimKey::Ctrl('h') => f.input.backspace(),
                        VimKey::Ctrl('w') => f.input.delete_word_back(),
                        _ => f.input.delete_to_line_start(),
                    });
                command.at = None;
                cx.notify();
                cx.stop_propagation();
                return;
            }
            Some(VimKey::Ctrl('r')) => {
                command.register = true;
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        if let Some(older) = history_key
            && !self.history.is_empty()
        {
            let last = self.history.len() - 1;
            command.at = match (older, command.at) {
                (true, None) => Some(last),
                (true, Some(at)) => Some(at.saturating_sub(1)),
                (false, Some(at)) if at < last => Some(at + 1),
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
            self.shown_bar = Some(format!(":{}", command.text));
            return Some(
                bar.gap_0()
                    .child(div().text_color(theme.fg).child(":"))
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
        // The `/` or `?` line being typed: Vim's state holds it, this shows it with a caret.
        if let Some(line) = self.code.read(cx).vim_command_line() {
            self.shown_bar = Some(line.clone());
            return Some(
                bar.text_color(theme.fg)
                    .child(
                        div()
                            .id("file-search-line")
                            .flex()
                            .items_center()
                            .min_w_0()
                            .child(div().min_w_0().truncate().child(line))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(2.0))
                                    .h(px(tokens.secondary_font_size + 2.0))
                                    .bg(theme.fg),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let message = self.code.read(cx).vim_message().cloned()?;
        self.shown_bar = Some(message.text.clone());
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

    /// What the line under the code showed in the last frame: the `:` line (`:w`), the `/` or
    /// `?` line (`/foo`), or the answer of a command; `None` when it was not drawn (tests).
    pub fn command_bar_text(&self) -> Option<&str> {
        self.shown_bar.as_deref()
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
        let Some(Conflict::Changed {
            disk: Some(disk), ..
        }) = buffer.conflict()
        else {
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
            ui::Button::new(id, label, theme, tokens)
                .compact()
                .selected(on)
                .tooltip(tip)
        };
        let step = |id: &'static str, icon: crate::icons::Icon, tip: &'static str| {
            ui::IconButton::new(id, icon, tip, theme, tokens).compact()
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
                ui::Button::new("file-replace-one", "Replace", theme, tokens)
                    .compact()
                    .tooltip("Replace this match (Return)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.replace_one(cx);
                    })),
            )
            .child(
                ui::Button::new("file-replace-all", "Replace all", theme, tokens)
                    .compact()
                    .tooltip("Replace every match, as one undo step (⌘Return)")
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
            Conflict::Changed { disk, by } => (
                match by {
                    Some(by) => {
                        format!("{by} changed {name} on disk, and your changes are not saved.")
                    }
                    None => format!("{name} changed on disk, and your changes are not saved."),
                },
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
                ui::Button::new("file-conflict-reload", "Reload", theme, tokens)
                    .compact()
                    .tooltip("Take the file from disk")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(FileViewEvent::Reload(reload.clone()));
                    })),
            );
        }
        let keep = path;
        actions = actions.child(
            ui::Button::new(
                "file-conflict-keep",
                if matches!(conflict, Conflict::Deleted) {
                    "Keep mine (save again)"
                } else {
                    "Keep mine (overwrite)"
                },
                theme,
                tokens,
            )
            .compact()
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(FileViewEvent::Overwrite(keep.clone()));
            })),
        );
        if can_diff {
            actions = actions.child(
                ui::Button::new(
                    "file-conflict-diff",
                    if self.diff.is_some() {
                        "Hide diff"
                    } else {
                        "Show diff"
                    },
                    theme,
                    tokens,
                )
                .compact()
                .ghost()
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
        let legend = |mark: &'static str, color: gpui::Hsla, what: &'static str| {
            div()
                .flex()
                .gap(px(look.tokens.spacing))
                .child(div().text_color(color).child(mark))
                .child(what)
        };
        let theme = &look.theme;
        div()
            .id("file-conflict-diff")
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .id("file-conflict-diff-legend")
                    .flex_none()
                    .flex()
                    .gap(px(look.tokens.spacing * 3.0))
                    .px(px(look.tokens.spacing * 2.0))
                    .py(px(look.tokens.spacing / 2.0))
                    .border_b_1()
                    .border_color(border)
                    .text_size(px(look.tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(legend("−", theme.diff_removed, "on disk"))
                    .child(legend("+", theme.diff_added, "yours, not saved")),
            )
            .child(self.render_diff_lines(lines, shown, more, &look))
            .into_any_element()
    }

    fn render_diff_lines(
        &self,
        lines: &[DiffLine],
        shown: usize,
        more: usize,
        look: &super::chat::Look,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("file-conflict-diff-lines")
            .flex_none()
            .max_h(px(280.0))
            .overflow_y_scroll()
            .font_family(look.mono.clone())
            .text_size(px(look.tokens.secondary_font_size))
            .children(
                lines[..shown]
                    .iter()
                    .map(|line| super::chat::diff_row(line, look)),
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
            ui::Button::new("file-open-system", label, theme, tokens)
                .kind(kind)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(FileViewEvent::OpenWithSystem(path.clone()));
                }))
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
                    .child(system_button("Open with system", ButtonKind::Secondary))
                    .child(
                        ui::Button::new("file-open-large", "Open read-only", theme, tokens)
                            .primary()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(FileViewEvent::OpenLarge(open_path.clone()));
                            })),
                    )
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
                    system_button("Open with system", ButtonKind::Primary).into_any_element()
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
        let lsp_bar = self
            .lsp
            .as_ref()
            .filter(|_| ready && self.lsp_bar_shown)
            .and_then(|lsp| {
                let path = self.buffer.read(cx).path().display().to_string();
                super::lsp_bar::render(lsp, &path, &self.regions, &theme, &tokens, cx)
            });
        let has_bar = conflict_bar.is_some() || agent_bar.is_some() || lsp_bar.is_some();
        if self.had_bar && !has_bar {
            self.code.update(cx, |code, _| code.keep_text_in_place());
        }
        self.had_bar = has_bar;
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
        self.shown_bar = None;
        let command_bar = ready
            .then(|| self.render_command_bar(&theme, &tokens, window, cx))
            .flatten();
        if command_bar.is_some() && !self.had_command_bar {
            self.code
                .update(cx, |code, cx| code.reveal_caret_next_frame(cx));
        }
        self.had_command_bar = command_bar.is_some();
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
            .on_action(cx.listener(|this, _: &ToggleBlame, _, cx| this.toggle_blame(cx)))
            .on_action(cx.listener(|this, _: &ToggleSoftWrap, _, cx| this.toggle_soft_wrap(cx)))
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
            .on_action(cx.listener(|this, _: &NextGitHunk, _, cx| {
                this.code.update(cx, |c, cx| c.next_git_hunk(cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousGitHunk, _, cx| {
                this.code.update(cx, |c, cx| c.previous_git_hunk(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleGitPeek, _, cx| {
                this.code.update(cx, |c, cx| c.toggle_git_peek(cx));
            }))
            .on_action(cx.listener(|this, _: &RevertGitHunk, _, cx| {
                this.code.update(cx, |c, cx| c.revert_git_hunk_at_caret(cx));
            }))
            .on_action(cx.listener(|this, _: &StageGitHunk, _, cx| {
                this.code.update(cx, |c, cx| c.stage_git_hunk_at_caret(cx));
            }))
            .children(conflict_bar)
            .children(agent_bar)
            .children(lsp_bar)
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
