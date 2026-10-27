//! Files in tabs (slice E-5): the buffer store (one [`FileBuffer`] per file, shared by every
//! tab that shows it, freed with the last one), opening a file — from ⌘P and the Files panel
//! (E-13a), the palette's "File: Open File…" (an Open panel that starts in the project), a
//! session's or a project's "Open File…", a ⌘-click on a path in a terminal
//! (`terminal.open_files_with = "editor"`), `hutan open` and `ui.run workbench::OpenFile` — and
//! the views of file tabs. Files are read through the daemon (`fs.stat`, `fs.read`): the one
//! place that decides what this window may read (ADR 0012, amendment decision 3). Saving,
//! watching the files for changes on disk and the conflict bar's answers are in
//! `file_sync.rs` (slice E-6). Every file opened goes first in the recent files ⌘P offers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, PathPromptOptions, Subscription,
    Task, Window,
};
use hutan_client::ClientError;
use hutan_code_view::{CodeStyle, FailReason, FileBuffer, LoadState};
use hutan_protocol::ErrorCode;
use hutan_protocol::fs::{FileVersion, FsKind};

use super::file_view::{FileView, FileViewEvent};
use super::model::{Edge, ItemRef, PaneId};
use super::view::{MenuEntry, Workbench};
use crate::settings::{EditorSettings, FilesPanelSettings};
use hutan_core::WorkspaceId;
use hutan_protocol::messages::WorkspaceKind;

/// A file to open, and where in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOpen {
    /// Absolute.
    pub path: PathBuf,
    /// 1-based.
    pub line: Option<u32>,
    /// 1-based, in characters.
    pub column: Option<u32>,
    /// The user chose the file (a dialog, a click, a restored tab): the daemon reads it from
    /// anywhere. Off (an agent's `ui.run`), only below the workspace roots.
    pub picked: bool,
}

impl FileOpen {
    pub fn picked(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            line: None,
            column: None,
            picked: true,
        }
    }
}

/// Where a file opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilePlace {
    /// In the focused pane (its tab when it has one), with the keyboard.
    #[default]
    Focused,
    /// In a new split right of the focused pane, with the keyboard (⌘Return in ⌘P).
    Split,
    /// In the focused pane, the keyboard staying where it is (Space or a click in the Files
    /// panel).
    Preview,
}

pub(crate) struct Stored {
    pub(crate) buffer: Entity<FileBuffer>,
    /// Vim's buffer number (`:b 2`, `:ls`): 1 for the first file opened in the window.
    pub(crate) number: usize,
    pub(crate) picked: bool,
    load: Option<Task<()>>,
    /// The version of the file the buffer holds (read or last saved): the precondition of the
    /// next save.
    pub(crate) disk: Option<FileVersion>,
    /// The file's version when the conflict was found: what *Keep mine* overwrites, and no
    /// newer change.
    pub(crate) conflict_disk: Option<FileVersion>,
    /// The file's text as last read or saved (E-10: what an agent's write is compared with);
    /// kept up to [`super::agent_edits::MAX_TEXT_BYTES`].
    pub(crate) disk_text: Option<std::sync::Arc<str>>,
    /// The file is watched for changes on disk.
    pub(crate) watched: bool,
    pub(crate) save: Option<Task<()>>,
    /// A read of the file after a change on disk.
    pub(crate) check: Option<Task<()>>,
    /// The pending `editor.autosave = "after_delay"` save.
    pub(crate) autosave: Option<Task<()>>,
    /// The blame being asked of the daemon (`blame.rs`).
    pub(crate) blame_task: Option<Task<()>>,
    /// The git base being fetched (`git_gutter.rs`).
    pub(crate) git_task: Option<Task<()>>,
    /// The file's checkout and its name there, as the last git base fetch found them.
    pub(crate) git_file: Option<(PathBuf, String)>,
    /// *Stage hunk* waiting for the save that makes the disk hold the buffer.
    pub(crate) stage_after_save: Option<super::git_gutter::PendingStage>,
    /// Whether the tabs showed the buffer as changed (they are drawn again when it flips).
    pub(crate) dirty: bool,
    _subscriptions: Vec<Subscription>,
}

/// One buffer per open file (by canonical path), alive while a tab shows it (the Agents
/// overview's stash counts).
#[derive(Default)]
pub struct BufferStore {
    pub(crate) buffers: HashMap<String, Stored>,
    /// The last two files whose code lost the keyboard, newest first: Vim's alternate file.
    pub(crate) recent: [Option<String>; 2],
    /// Takes the connection's `fs.changed` notices while there are buffers.
    pub(crate) listener: Option<Task<()>>,
    /// `fs.changed` notices handled so far (tests wait on it).
    pub(crate) changes_seen: u64,
    /// The number the last buffer got.
    pub(crate) last_number: usize,
}

impl std::fmt::Debug for BufferStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferStore")
            .field("buffers", &self.buffers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl BufferStore {
    /// The buffer of `path`, if a tab holds it.
    pub fn get(&self, path: &str) -> Option<&Entity<FileBuffer>> {
        self.buffers.get(path).map(|s| &s.buffer)
    }

    /// How many buffers are alive.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// The `fs.changed` notices this window has handled (tests).
    pub fn changes_seen(&self) -> u64 {
        self.changes_seen
    }
}

/// What a failed read means for the tab.
pub(crate) fn fail_reason(error: &ClientError) -> FailReason {
    let ClientError::Remote(body) = error else {
        return FailReason::Other;
    };
    let reason = body
        .data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(serde_json::Value::as_str);
    match (body.code, reason) {
        (ErrorCode::Unsupported, Some("binary")) => FailReason::Binary,
        (ErrorCode::Unsupported, Some("not_utf8")) => FailReason::NotUtf8,
        (ErrorCode::Unsupported, Some("too_large")) => FailReason::TooLarge,
        (ErrorCode::Unsupported, Some("not_a_file")) => FailReason::NotAFile,
        (ErrorCode::NotFound, _) => FailReason::Missing,
        (ErrorCode::Forbidden, _) => FailReason::Forbidden,
        _ => FailReason::Other,
    }
}

pub(crate) fn error_text(error: &ClientError) -> String {
    match error {
        ClientError::Remote(body) => body.message.clone(),
        other => other.to_string(),
    }
}

impl Workbench {
    /// The buffers of the open files (tests, the status bar).
    pub fn buffers(&self) -> &BufferStore {
        &self.buffers
    }

    /// The `editor.position` and `editor.mode` status item views, once the status bar was
    /// drawn (tests).
    pub fn editor_items(&self) -> Option<&super::editor_status::EditorItems> {
        self.editor_items.as_ref()
    }

    /// The view of the file tab of `path` in `pane`, if it was drawn.
    pub fn file_view_of(&self, pane: PaneId, path: &str) -> Option<Entity<FileView>> {
        self.file_views.get(&(pane, path.to_owned())).cloned()
    }

    /// The view of the focused pane's active tab, if it is a file.
    pub fn focused_file_view(&self) -> Option<Entity<FileView>> {
        let pane = self.state.active_pane_id();
        let item = self.state.active_pane()?.active_item()?.clone();
        let path = item.as_file()?;
        self.file_views.get(&(pane, path.to_owned())).cloned()
    }

    /// Opens `target` in a tab (or focuses the tab that shows it) and brings its line into view.
    /// The daemon is asked first what the path is: its canonical form is the tab's identity.
    pub fn open_file(&mut self, target: FileOpen, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_placed(target, FilePlace::Focused, window, cx);
    }

    /// [`Self::open_file`] at `place`.
    pub fn open_file_placed(
        &mut self,
        target: FileOpen,
        place: FilePlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.sessions.read(cx).connection().clone();
        if !connection.supports_fs() {
            self.set_notice(
                "This hutand cannot read files for the editor (it is older than protocol 0.33); restart it",
                cx,
            );
            return;
        }
        let stat = cx.background_spawn({
            let path = target.path.clone();
            let picked = target.picked;
            async move { connection.fs_stat(&path, picked).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let entry = stat.await;
            this.update_in(cx, |this, window, cx| match entry {
                Ok(entry) if entry.kind == FsKind::File => {
                    this.show_file(&entry.path, entry.size, &target, place, window, cx);
                }
                Ok(entry) => this.set_notice(format!("{} is not a file", entry.path.display()), cx),
                Err(error) => {
                    if fail_reason(&error) == FailReason::Missing {
                        // A recent file that is gone is not offered again.
                        let gone = target.path.display().to_string();
                        this.recent_files.retain(|f| *f != gone);
                    }
                    this.set_notice(
                        format!(
                            "Cannot open {}: {}",
                            target.path.display(),
                            error_text(&error)
                        ),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows the file at canonical `path` in a tab: focuses the tab that has it, else adds one to
    /// the focused pane (in place of a lone placeholder), or to a new split right of it.
    pub(crate) fn show_file(
        &mut self,
        path: &Path,
        size: u64,
        target: &FileOpen,
        place: FilePlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = path.display().to_string();
        // Opening a file closes a peek (its focus moves to the file's pane); a preview — Space or
        // a click in the Files tree — leaves the keyboard in the tree, so the peek stays.
        let peeked = self.peek_dock().filter(|_| place == FilePlace::Preview);
        super::persistence::note_recent_file(&mut self.recent_files, &key);
        self.ensure_buffer(&key, target.picked, Some(size), cx);
        let item = ItemRef::file(key.clone());
        if place == FilePlace::Split {
            let pane = self.state.active_pane_id();
            self.try_mutate(cx, |s| s.split_pane(pane, Edge::Right, item.clone()));
        } else if let Some((pane, index)) = self.state.find_item(&item) {
            self.try_mutate(cx, |s| {
                s.set_active_item(pane, index)?;
                s.focus_pane(pane)
            });
        } else {
            let pane = self.state.active_pane_id();
            let placeholder = self
                .state
                .pane(pane)
                .and_then(|p| match p.items.as_slice() {
                    [only @ ItemRef::Placeholder { .. }] => Some(only.clone()),
                    _ => None,
                });
            self.try_mutate(cx, |s| {
                if let Some(placeholder) = &placeholder
                    && s.replace_item(placeholder, item.clone())
                {
                    return s.focus_pane(pane);
                }
                s.add_item(pane, item.clone()).map(|_| ())
            });
        }
        if let Some((pane, _)) = self.state.find_item(&item) {
            let view = self.file_view(pane, &key, window, cx);
            if let Some(line) = target.line {
                let column = target.column.map(|c| c as usize);
                let code = view.read(cx).code().clone();
                code.update(cx, |code, cx| code.reveal_line(line as usize, column, cx));
            }
        }
        if let Some(pos) = peeked {
            self.peek = Some(pos);
            self.state.focus = super::model::FocusTarget::Dock(pos);
        }
        if place != FilePlace::Preview {
            self.focus_root(window, cx);
        }
    }

    /// The buffer of `path`, made (and its file read) when there is none.
    pub(crate) fn ensure_buffer(
        &mut self,
        path: &str,
        picked: bool,
        size: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Entity<FileBuffer> {
        if let Some(stored) = self.buffers.buffers.get(path) {
            return stored.buffer.clone();
        }
        let buffer = cx.new(|cx| FileBuffer::new(PathBuf::from(path), cx));
        let editor = self.settings.get::<EditorSettings>();
        let (highlight, git_gutter) = (editor.agent_highlight, editor.git_gutter);
        buffer.update(cx, |b, cx| {
            b.set_agent_highlight(highlight, cx);
            b.set_git_gutter(git_gutter, cx);
        });
        if self.install.holds(Path::new(path)) {
            buffer.update(cx, |b, cx| b.set_read_only(true, cx));
        }
        let subscriptions = self.watch_buffer(path, &buffer, cx);
        self.buffers.last_number += 1;
        self.buffers.buffers.insert(
            path.to_owned(),
            Stored {
                buffer: buffer.clone(),
                number: self.buffers.last_number,
                picked,
                load: None,
                disk: None,
                conflict_disk: None,
                disk_text: None,
                watched: false,
                save: None,
                check: None,
                autosave: None,
                blame_task: None,
                git_task: None,
                git_file: None,
                stage_after_save: None,
                dirty: false,
                _subscriptions: subscriptions,
            },
        );
        self.listen_for_file_changes(cx);
        self.load_buffer(path, size, false, cx);
        buffer
    }

    /// Reads the file of `path` into its buffer. Above `editor.large_file_bytes` nothing is read
    /// until the user confirms (`confirmed`), and then the buffer is in large-file mode.
    fn load_buffer(
        &mut self,
        path: &str,
        size: Option<u64>,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        let picked = stored.picked;
        let limit = self.settings.get::<EditorSettings>().large_file_bytes;
        let connection = self.sessions.read(cx).connection().clone();
        let file = PathBuf::from(path);
        buffer.update(cx, |b, cx| b.set_state(LoadState::Loading, cx));
        let read = cx.background_spawn(async move {
            let size = match size {
                Some(size) => size,
                None => connection.fs_stat(&file, picked).await?.size,
            };
            if size > limit && !confirmed {
                return Ok((size, None));
            }
            connection
                .fs_read_all(&file, picked)
                .await
                .map(|text| (size, Some(text)))
        });
        let key = path.to_owned();
        stored.load = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            let loaded = buffer.update(cx, |b, cx| match read {
                Ok((size, None)) => {
                    b.set_size(size);
                    b.set_state(LoadState::NeedsConfirm { size }, cx);
                    None
                }
                Ok((size, Some(read))) => {
                    b.set_size(size);
                    b.set_loaded(&read.text, size > limit, cx);
                    Some((read.version, read.text))
                }
                Err(error) => {
                    b.set_state(
                        LoadState::Failed {
                            reason: fail_reason(&error),
                            message: error_text(&error),
                        },
                        cx,
                    );
                    None
                }
            });
            this.update(cx, |this, cx| {
                if let Some((version, text)) = loaded {
                    this.loaded(&key, version, Some(&text), cx);
                }
                // A Replace All may wait for this buffer (read or not).
                this.replace_buffer_ready(&key, cx);
                this.replace_maybe_done(cx);
            })
            .ok();
        }));
    }

    /// The user confirmed opening a large file.
    fn open_large(&mut self, path: &Path, cx: &mut Context<Self>) {
        let key = path.display().to_string();
        let size = self.buffers.get(&key).map(|b| b.read(cx).size());
        self.load_buffer(&key, size, true, cx);
    }

    /// The view of the file tab of `path` in `pane`, made on first use.
    pub(crate) fn file_view(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FileView> {
        let key = (pane, path.to_owned());
        if let Some(view) = self.file_views.get(&key) {
            return view.clone();
        }
        // A restored tab is the user's own: read it as picked.
        let buffer = self.ensure_buffer(path, true, None, cx);
        let style = self.code_style(cx);
        let settings = self.settings.clone();
        let regions = self.regions.clone();
        let view = cx.new(|cx| FileView::new(settings, regions, buffer, style, cx));
        let lsp = self.lsp.clone();
        view.update(cx, |view, cx| view.set_lsp(lsp, cx));
        // A caret move or an edit is no event here: the editor's status items read the code
        // view themselves (`editor_status.rs`), so the workbench is not notified (B12a).
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &FileViewEvent, window, cx| match event {
                FileViewEvent::OpenWithSystem(path) => {
                    super::open_link::open_with_system(path, cx);
                }
                FileViewEvent::OpenLarge(path) => this.open_large(path, cx),
                FileViewEvent::Save(path) => {
                    this.save_file(&path.display().to_string(), None, cx);
                }
                FileViewEvent::Reload(path) => this.reload_file(&path.display().to_string(), cx),
                FileViewEvent::Overwrite(path) => {
                    this.overwrite_file(&path.display().to_string(), cx);
                }
                FileViewEvent::FocusLost(path) => {
                    let key = path.display().to_string();
                    let recent = &mut this.buffers.recent;
                    if recent[0].as_deref() != Some(key.as_str()) {
                        recent[1] = recent[0].replace(key);
                    }
                    this.autosave_on_focus_change(&path.display().to_string(), cx);
                }
                FileViewEvent::Ex(command) => {
                    this.run_file_command(pane, view, command.clone(), window, cx);
                }
                FileViewEvent::AcceptAgentChanges(path) => {
                    this.accept_agent_changes(&path.display().to_string(), cx);
                }
                FileViewEvent::BlameChanged(path) => {
                    this.refresh_blame(&path.display().to_string(), cx);
                }
                FileViewEvent::GitHunkStage {
                    path,
                    first_line,
                    end_line,
                } => this.stage_git_hunk(
                    &path.display().to_string(),
                    super::git_gutter::PendingStage {
                        first_line: *first_line,
                        end_line: *end_line,
                    },
                    cx,
                ),
                // The status bar's `Wrap` follows the tab.
                FileViewEvent::WrapChanged => cx.notify(),
                FileViewEvent::OpenCommit { path, commit } => {
                    this.open_file_history(path, Some(commit.clone()), window, cx);
                }
                FileViewEvent::Hover { offset } => this.lsp_hover(view, *offset, cx),
                FileViewEvent::Definition { offset } => {
                    this.go_to_definition(view, *offset, window, cx);
                }
                FileViewEvent::GoBack | FileViewEvent::TagBack => this.go_back(window, cx),
                FileViewEvent::TagJump { offset } => {
                    this.tag_jump(view, *offset, window, cx);
                }
                FileViewEvent::AlternateFile => this.alternate_file(view, window, cx),
                FileViewEvent::VimWindow(command) => {
                    this.vim_window(pane, view, *command, window, cx);
                }
                FileViewEvent::Completion {
                    request,
                    offset,
                    trigger,
                    incomplete,
                } => this.lsp_completion(view, *request, *offset, *trigger, *incomplete, cx),
                FileViewEvent::CompletionResolve { list, index } => {
                    this.lsp_completion_resolve(view, *list, *index, cx);
                }
                FileViewEvent::SignatureHelp {
                    request,
                    offset,
                    trigger,
                    retrigger,
                } => this.lsp_signature_help(view, *request, *offset, *trigger, *retrigger, cx),
            },
        );
        self.file_subscriptions.insert(key.clone(), subscription);
        self.file_views.insert(key, view.clone());
        self.refresh_completion_triggers(cx);
        // `editor.inline_blame` is on: this tab's buffer is blamed (asked again when its text
        // arrives, if it has not).
        if view.read(cx).wants_blame() {
            self.refresh_blame(path, cx);
        }
        self.sync_git_stage_availability(path, cx);
        self.sync_lsp_bars(cx);
        view
    }

    /// Body of a file tab.
    pub(crate) fn render_file_item(
        &mut self,
        pane: PaneId,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.rendered_files.insert((pane, path.to_owned()));
        let view = self.file_view(pane, path, window, cx);
        super::render::cached_view(&self.regions, view)
    }

    /// Every `(pane, path)` that has a file tab in the center, in tree order.
    fn open_file_items(&self) -> Vec<(PaneId, String)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items
                    .iter()
                    .filter_map(move |item| item.as_file().map(|p| (pane.id, p.to_owned())))
            })
            .collect()
    }

    /// Drops the views of file tabs that are gone and the buffers no tab holds any more (the
    /// Agents overview's stash keeps its files' buffers); keeps a syntax tree only for the files
    /// a pane shows.
    pub(crate) fn sync_files(&mut self, cx: &mut Context<Self>) {
        let items = self.open_file_items();
        let mut wanted: HashSet<String> = items.iter().map(|(_, path)| path.clone()).collect();
        if let Some(saved) = &self.state.stashed {
            for pane in saved.center.panes() {
                wanted.extend(
                    pane.items
                        .iter()
                        .filter_map(|i| i.as_file().map(str::to_owned)),
                );
            }
        }
        let keep: HashSet<(PaneId, String)> = items.into_iter().collect();
        let gone: Vec<(PaneId, String)> = self
            .file_views
            .keys()
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            self.file_subscriptions.remove(&key);
            if let Some(view) = self.file_views.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned_files.push(view);
            }
        }
        let dropped: Vec<(String, bool, bool)> = self
            .buffers
            .buffers
            .iter()
            .filter(|(path, _)| !wanted.contains(*path))
            .map(|(path, stored)| {
                let b = stored.buffer.read(cx);
                // Hunks not computed yet count as left: the ledger keeps the file.
                let hunks_left = !b.agent_hunks().is_empty() || !b.agent_hunks_current();
                (path.clone(), stored.watched, hunks_left)
            })
            .collect();
        let mut dropped_paths = Vec::new();
        for (path, watched, hunks_left) in dropped {
            self.buffers.buffers.remove(&path);
            self.lsp.update(cx, |lsp, cx| lsp.close(&path, cx));
            if watched {
                // The ledger keeps watching a file with an agent's changes.
                self.ledger_tab_closed(&path, hunks_left, cx);
            }
            dropped_paths.push(path);
        }
        if !dropped_paths.is_empty() {
            self.replace_buffers_dropped(&dropped_paths, cx);
        }
        if self.buffers.buffers.is_empty() && self.agent_ledger.watched.is_empty() {
            self.buffers.listener = None;
        }
        let shown: HashSet<&str> = self
            .state
            .center
            .panes()
            .into_iter()
            .filter_map(|pane| pane.active_item().and_then(ItemRef::as_file))
            .collect();
        let buffers: Vec<(String, Entity<FileBuffer>, bool)> = self
            .buffers
            .buffers
            .iter()
            .map(|(path, stored)| {
                (
                    path.clone(),
                    stored.buffer.clone(),
                    shown.contains(path.as_str()),
                )
            })
            .collect();
        let mut shown_again = Vec::new();
        for (path, buffer, visible) in buffers {
            if visible && !buffer.read(cx).syntax_active() {
                shown_again.push(path);
            }
            buffer.update(cx, |b, cx| b.set_syntax_active(visible, cx));
        }
        self.sync_lsp_bars(cx);
        // The index or `HEAD` may have moved while the tab was behind another.
        for path in shown_again {
            self.refresh_git_base(&path, cx);
        }
        self.follow_active_file(cx);
        self.follow_active_changes(cx);
    }

    /// How code looks now: the editor colors of the chrome's pack, its monospace family, and
    /// the `editor` font settings.
    pub(crate) fn code_style(&self, cx: &gpui::App) -> CodeStyle {
        let editor = self.settings.get::<EditorSettings>();
        CodeStyle::new(
            self.resolved_theme.editor(),
            super::terminals::mono_family(cx, &self.resolved_theme),
            editor.font_size as f32,
            editor.line_height as f32,
        )
    }

    /// Restyles every file tab (a theme, appearance or `editor` font change); nothing is parsed
    /// again.
    pub(crate) fn refresh_code_style(&mut self, cx: &mut Context<Self>) {
        let style = self.code_style(cx);
        for view in self.file_views.values() {
            view.update(cx, |view, cx| view.set_style(style.clone(), cx));
        }
        self.restyle_revisions(&style, cx);
    }

    /// What the editor status items show at the workbench's level for the focused tab: its
    /// language and whether Vim is on (its line, column and Vim mode are read by the items).
    pub fn editor_status(&self, cx: &gpui::App) -> Option<super::registry::EditorStatus> {
        let view = self.focused_file_view()?;
        let view = view.read(cx);
        let buffer = view.buffer().read(cx);
        if !buffer.is_ready() {
            return None;
        }
        let mut language = hutan_code_view::language_label(buffer.language()).to_owned();
        if buffer.is_large() {
            language.push_str(" · large file");
        }
        let path = buffer.path().display().to_string();
        Some(super::registry::EditorStatus {
            language,
            vim: self.vim_config().enabled,
            wrap: view.wraps(cx),
            diagnostics: self.editor_diagnostics(&path, cx),
        })
    }

    /// "File: Open File…": a file picked in an Open panel that starts in the active project's
    /// folder (Home's folder at Home).
    pub(crate) fn open_file_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = {
            let store = self.sessions.read(cx);
            store.files_root().or_else(|| {
                store
                    .home_workspace()
                    .and_then(|home| home.roots.first().cloned())
            })
        };
        let panel = self.mac.open_panel.clone();
        cx.spawn_in(window, async move |this, cx| {
            // AppKit is asked outside of any update of the window, as GPUI does with its own.
            let native = start.as_deref().and_then(|dir| panel.choose_file(dir));
            let path = match native {
                Some(chosen) => chosen.await.ok().flatten(),
                None => {
                    let Ok(chosen) = cx.update(|_, cx| {
                        cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Open".into()),
                        })
                    }) else {
                        return;
                    };
                    match chosen.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    }
                }
            };
            let Some(path) = path else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.open_file(FileOpen::picked(path), window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// "Open File…" of a session or a project row: the same panel, starting in the active
    /// project.
    pub(crate) fn open_file_entry() -> MenuEntry {
        Self::entry("Open File…", |this, window, cx| {
            this.open_file_dialog(window, cx);
        })
    }

    /// The file of `path` in a new split of `pane` at `edge`: a second view of the same buffer.
    /// The new view starts at the caret and the scroll of the view it was split from, as Vim's
    /// `:sp` does, and is made now rather than at its first frame, so it has the keyboard at
    /// once (its `:` line, its `:q`).
    pub fn open_file_in_split(
        &mut self,
        pane: PaneId,
        path: &str,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = self
            .file_view_of(pane, path)
            .map(|view| view.read(cx).code().clone());
        let item = ItemRef::file(path);
        if let Some(split) = self.try_mutate(cx, |s| s.split_pane(pane, edge, item)) {
            let view = self.file_view(split, path, window, cx);
            if let Some(from) = from {
                let code = view.read(cx).code().clone();
                code.update(cx, |code, cx| code.start_like(&from, cx));
                // The view split from is smaller now: its caret stays in view too.
                from.update(cx, |code, cx| code.reveal_caret_next_frame(cx));
            }
        }
        self.focus_root(window, cx);
    }

    /// "File: Open in Split Right / Down": the focused file tab's file in a new split.
    pub(crate) fn split_focused_file(
        &mut self,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.state.active_pane_id();
        let path = self
            .state
            .active_pane()
            .and_then(|p| p.active_item())
            .and_then(ItemRef::as_file)
            .map(str::to_owned);
        match path {
            Some(path) => self.open_file_in_split(pane, &path, edge, window, cx),
            None => self.set_notice("The focused tab is not a file", cx),
        }
    }

    /// Entries of a file tab's menu: the same file in a new split (one buffer, two views), and
    /// its path.
    pub(crate) fn file_tab_entries(&self, pane: PaneId, path: &str) -> Vec<MenuEntry> {
        let split = |label: &'static str, edge: Edge, path: String| {
            Self::entry(label, move |this, window, cx| {
                this.open_file_in_split(pane, &path, edge, window, cx);
            })
        };
        let copy = path.to_owned();
        let reveal = path.to_owned();
        let blame = path.to_owned();
        vec![
            split("Open in Split Right", Edge::Right, path.to_owned()),
            split("Open in Split Down", Edge::Bottom, path.to_owned()),
            Self::entry("Reveal in Files Panel", move |this, window, cx| {
                this.show_files_at(Some(reveal.clone()), window, cx);
            }),
            Self::entry("Toggle Blame", move |this, _, cx| {
                this.toggle_blame_of(pane, &blame, cx);
            }),
            Self::show_history_entry(path),
            Self::entry("Copy Path", move |this, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                this.set_notice("Copied the file's path", cx);
            }),
        ]
    }

    // ---- the Files panel (E-13a) -----------------------------------------------------------

    /// The file of the center's active tab, if it is one.
    pub(crate) fn active_center_file(&self) -> Option<String> {
        self.state
            .active_center_item()
            .and_then(|item| item.as_file().map(str::to_owned))
    }

    /// Tells the Files panel which file the focused tab shows (`panels.files.auto_reveal`),
    /// once per change; a panel not made yet hears of it when it is.
    fn follow_active_file(&mut self, cx: &mut Context<Self>) {
        let file = self.active_center_file();
        if file == self.revealed_file {
            return;
        }
        let Some(panel) = self.panel::<super::panels::FilesPanel>("files") else {
            return;
        };
        self.revealed_file = file.clone();
        let Some(file) = file else {
            return;
        };
        if self.settings.get::<FilesPanelSettings>().auto_reveal {
            panel.update(cx, |panel, cx| panel.reveal(Path::new(&file), cx));
        }
    }

    /// The Files panel, shown (in its default dock when it is closed).
    fn files_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<super::panels::FilesPanel>> {
        self.open_panel("files", cx);
        let view = self.panel_view(&super::model::PanelId::new("files"), window, cx)?;
        view.downcast::<super::panels::FilesPanel>().ok()
    }

    /// ⌘⇧E: the Files panel with the keyboard in it, the focused tab's file selected; with the
    /// keyboard in the tree already, back to what the focused tab shows (the panel stays open).
    pub(crate) fn show_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files_tree_has_focus(window, cx) {
            self.focus_editor(window, cx);
            return;
        }
        let file = self.active_center_file();
        self.show_files_at(file, window, cx);
    }

    /// Whether the keyboard is in the Files tree.
    pub(crate) fn files_tree_has_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.panel::<super::panels::FilesPanel>("files")
            .is_some_and(|panel| panel.read(cx).has_focus(window, cx))
    }

    /// Gives the keyboard back to what the focused pane shows, from a dock.
    pub(crate) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        self.focus_pane(pane, window, cx);
    }

    /// ⌘W with the keyboard in the Files tree: closes the active tab of the center (asking about
    /// unsaved changes as it always does) and leaves the keyboard and the panel where they are.
    pub(crate) fn close_center_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        let active = self
            .state
            .pane(pane)
            .filter(|p| p.active < p.items.len())
            .map(|p| p.active);
        if let Some(index) = active {
            self.close_item_checked(pane, index, window, cx);
        }
    }

    /// *Files: Collapse All*.
    pub(crate) fn files_collapse_all(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel::<super::panels::FilesPanel>("files") {
            panel.update(cx, |panel, cx| panel.collapse_all(cx));
        }
    }

    /// *Files: Refresh*.
    pub(crate) fn files_refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel::<super::panels::FilesPanel>("files") {
            panel.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    /// The Files panel's ⋯ menu: what the tree lists (hidden files, ignored files), how it
    /// folds and draws, and the tree-wide actions.
    pub(crate) fn show_files_more_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::settings::IndentGuides;
        let Some(panel) = self.panel::<super::panels::FilesPanel>("files") else {
            return;
        };
        let now = self.settings.get::<FilesPanelSettings>();
        let toggle = |label: &'static str,
                      key: &'static str,
                      on: bool,
                      panel: &Entity<super::panels::FilesPanel>| {
            let panel = panel.clone();
            let mut entry = Self::entry(label, move |_, _, cx| {
                panel.update(cx, |panel, cx| {
                    panel.set_option(key, toml::Value::Boolean(!on), cx);
                });
            });
            entry.checked = on;
            entry
        };
        let guides = |label: &'static str,
                      value: &'static str,
                      on: bool,
                      panel: &Entity<super::panels::FilesPanel>| {
            let panel = panel.clone();
            let mut entry = Self::entry(label, move |_, _, cx| {
                panel.update(cx, |panel, cx| {
                    panel.set_option("indent_guides", toml::Value::String(value.to_owned()), cx);
                });
            });
            entry.checked = on;
            entry
        };
        let (expand, collapse, refresh) = (panel.clone(), panel.clone(), panel.clone());
        let entries = vec![
            toggle("Show Hidden Files", "show_hidden", now.show_hidden, &panel),
            toggle(
                "Show Ignored Files",
                "show_ignored",
                now.show_ignored,
                &panel,
            ),
            toggle(
                "Fold Single-Folder Chains",
                "compact_folders",
                now.compact_folders,
                &panel,
            ),
            guides(
                "Indent Guides on Hover",
                "hover",
                now.indent_guides == IndentGuides::Hover,
                &panel,
            ),
            guides(
                "Indent Guides Always",
                "always",
                now.indent_guides == IndentGuides::Always,
                &panel,
            ),
            guides(
                "No Indent Guides",
                "off",
                now.indent_guides == IndentGuides::Off,
                &panel,
            ),
            Self::entry("Expand All", move |_, window, cx| {
                expand.update(cx, |panel, cx| panel.expand_all(window, cx));
            }),
            Self::entry("Collapse All", move |_, _, cx| {
                collapse.update(cx, |panel, cx| panel.collapse_all(cx));
            }),
            Self::entry("Refresh", move |_, _, cx| {
                refresh.update(cx, |panel, cx| panel.refresh(cx));
            }),
        ];
        self.open_menu(position, entries, window, cx);
    }

    /// The Files panel with the keyboard in it and `file` (when there is one) selected.
    pub(crate) fn show_files_at(
        &mut self,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.files_panel(window, cx) else {
            self.set_notice("The Files panel is not available", cx);
            return;
        };
        // The panel is drawn (and its tree can take the keyboard) after this frame.
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| {
                if let Some(file) = &file {
                    panel.reveal(Path::new(file), cx);
                }
                panel.focus_tree(window, cx);
            });
        });
    }

    /// The project whose repository has a checkout at `path`: `(project, the checkout's path as
    /// the store has it, whether it is the main checkout)`.
    fn project_of_checkout(
        &self,
        path: &Path,
        cx: &gpui::App,
    ) -> Option<(WorkspaceId, PathBuf, bool)> {
        let canonical = std::fs::canonicalize(path).ok();
        let store = self.sessions.read(cx);
        store
            .workspaces()
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Project)
            .find_map(|project| {
                store
                    .checkouts_of(project)
                    .into_iter()
                    .find(|c| c.path == path || canonical.as_deref() == Some(c.path.as_path()))
                    .map(|c| (project.id, c.path, c.main))
            })
    }

    /// Makes the Files panel (and Search) work in the checkout at `root` of `project`: the
    /// worktree is picked in the sidebar, the main checkout is the project's own folder. A
    /// checkout of another project needs `window` to show that project first.
    fn scope_files_to(
        &mut self,
        project: WorkspaceId,
        root: &Path,
        main: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.read(cx).active_project_id() != Some(project) {
            let Some(window) = window else {
                return;
            };
            self.switch_workspace(project, window, cx);
        }
        let root = root.to_owned();
        self.sessions.update(cx, |store, cx| {
            store.set_active_checkout(project, (!main).then_some(root), cx);
        });
    }

    /// *Show in Files* of the Changes view: the Files panel in the checkout `checkout`, with its
    /// file `file` (relative to it) selected.
    pub(crate) fn show_in_files(
        &mut self,
        checkout: &Path,
        file: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((project, root, main)) = self.project_of_checkout(checkout, cx) else {
            self.set_notice(
                "Files shows the checkouts of your projects; this one is not",
                cx,
            );
            return;
        };
        self.scope_files_to(project, &root, main, Some(window), cx);
        let absolute = root.join(file).display().to_string();
        self.show_files_at(Some(absolute), window, cx);
    }

    /// While a Changes tab is the focused one, the Files panel works in the checkout it shows
    /// (once per change of tab; only within the project the window works in, so following
    /// never swaps the layout).
    fn follow_active_changes(&mut self, cx: &mut Context<Self>) {
        let shown = self.state.active_center_item().and_then(|item| match item {
            ItemRef::Changes { path } => Some(path.clone()),
            _ => None,
        });
        if shown == self.followed_changes {
            return;
        }
        self.followed_changes = shown.clone();
        let Some(path) = shown else {
            return;
        };
        if let Some((project, root, main)) = self.project_of_checkout(Path::new(&path), cx) {
            self.scope_files_to(project, &root, main, None, cx);
        }
    }

    /// *Files: Reveal Active File*: the focused tab's file selected in the Files panel.
    pub(crate) fn reveal_active_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.active_center_file() else {
            self.set_notice("The focused tab is not a file", cx);
            return;
        };
        // Not `show_files`: from the tree it would hand the keyboard back instead.
        self.show_files_at(Some(file), window, cx);
    }
}
