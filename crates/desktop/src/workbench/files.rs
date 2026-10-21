//! Files in tabs (slice E-5): the buffer store (one [`FileBuffer`] per file, shared by every
//! tab that shows it, freed with the last one), opening a file — from the palette's "File: Open
//! File…", a session's or a project's "Open File…", a ⌘-click on a path in a terminal
//! (`terminal.open_files_with = "editor"`), `hutan open` and `ui.run workbench::OpenFile` — and
//! the views of file tabs. Files are read through the daemon (`fs.stat`, `fs.read`): the one
//! place that decides what this window may read (ADR 0012, amendment decision 3).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, PathPromptOptions, Task, Window,
};
use hutan_client::ClientError;
use hutan_code_view::{CodeStyle, FailReason, FileBuffer, LoadState};
use hutan_protocol::ErrorCode;
use hutan_protocol::fs::FsKind;

use super::file_view::{FileView, FileViewEvent};
use super::model::{Edge, ItemRef, PaneId};
use super::view::{MenuEntry, Workbench};
use crate::settings::EditorSettings;

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

struct Stored {
    buffer: Entity<FileBuffer>,
    picked: bool,
    load: Option<Task<()>>,
}

/// One buffer per open file (by canonical path), alive while a tab shows it (the Agents
/// overview's stash counts).
#[derive(Default)]
pub struct BufferStore {
    buffers: HashMap<String, Stored>,
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
}

/// What a failed read means for the tab.
fn fail_reason(error: &ClientError) -> FailReason {
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

fn error_text(error: &ClientError) -> String {
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
                    this.show_file(&entry.path, entry.size, &target, window, cx);
                }
                Ok(entry) => this.set_notice(format!("{} is not a file", entry.path.display()), cx),
                Err(error) => this.set_notice(
                    format!(
                        "Cannot open {}: {}",
                        target.path.display(),
                        error_text(&error)
                    ),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Shows the file at canonical `path` in a tab: focuses the tab that has it, else adds one to
    /// the focused pane (in place of a lone placeholder).
    fn show_file(
        &mut self,
        path: &Path,
        size: u64,
        target: &FileOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = path.display().to_string();
        self.ensure_buffer(&key, target.picked, Some(size), cx);
        let item = ItemRef::file(key.clone());
        if let Some((pane, index)) = self.state.find_item(&item) {
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
            let view = self.file_view(pane, &key, cx);
            if let Some(line) = target.line {
                let column = target.column.map(|c| c as usize);
                let code = view.read(cx).code().clone();
                code.update(cx, |code, cx| code.reveal_line(line as usize, column, cx));
            }
        }
        self.focus_root(window, cx);
    }

    /// The buffer of `path`, made (and its file read) when there is none.
    fn ensure_buffer(
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
        self.buffers.buffers.insert(
            path.to_owned(),
            Stored {
                buffer: buffer.clone(),
                picked,
                load: None,
            },
        );
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
                .map(|text| (size, Some(text.text)))
        });
        stored.load = Some(cx.spawn(async move |_, cx| {
            let read = read.await;
            buffer.update(cx, |b, cx| match read {
                Ok((size, None)) => {
                    b.set_size(size);
                    b.set_state(LoadState::NeedsConfirm { size }, cx);
                }
                Ok((size, Some(text))) => {
                    b.set_size(size);
                    b.set_loaded(&text, size > limit, cx);
                }
                Err(error) => b.set_state(
                    LoadState::Failed {
                        reason: fail_reason(&error),
                        message: error_text(&error),
                    },
                    cx,
                ),
            });
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
        let subscription = cx.subscribe(&view, |this, _, event: &FileViewEvent, cx| match event {
            FileViewEvent::OpenWithSystem(path) => super::open_link::open_with_system(path, cx),
            FileViewEvent::OpenLarge(path) => this.open_large(path, cx),
            FileViewEvent::Changed => cx.notify(),
        });
        self.file_subscriptions.insert(key.clone(), subscription);
        self.file_views.insert(key, view.clone());
        view
    }

    /// Body of a file tab.
    pub(crate) fn render_file_item(
        &mut self,
        pane: PaneId,
        path: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.rendered_files.insert((pane, path.to_owned()));
        let view = self.file_view(pane, path, cx);
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
        self.buffers.buffers.retain(|path, _| wanted.contains(path));
        let shown: HashSet<&str> = self
            .state
            .center
            .panes()
            .into_iter()
            .filter_map(|pane| pane.active_item().and_then(ItemRef::as_file))
            .collect();
        let buffers: Vec<(Entity<FileBuffer>, bool)> = self
            .buffers
            .buffers
            .iter()
            .map(|(path, stored)| (stored.buffer.clone(), shown.contains(path.as_str())))
            .collect();
        for (buffer, visible) in buffers {
            buffer.update(cx, |b, cx| b.set_syntax_active(visible, cx));
        }
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
    }

    /// What the `editor.position` and `editor.language` status items show for the focused tab.
    pub(crate) fn editor_status(&self, cx: &gpui::App) -> Option<super::registry::EditorStatus> {
        let view = self.focused_file_view()?;
        let view = view.read(cx);
        let buffer = view.buffer().read(cx);
        if !buffer.is_ready() {
            return None;
        }
        let (line, column) = view.code().read(cx).position(cx);
        let mut language = hutan_code_view::language_label(buffer.language()).to_owned();
        if buffer.is_large() {
            language.push_str(" · large file");
        }
        Some(super::registry::EditorStatus {
            position: format!("Ln {line}, Col {column}"),
            language,
        })
    }

    /// "File: Open File…": a file picked in Finder.
    pub(crate) fn open_file_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.open_file(FileOpen::picked(path), window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// "Open File…" of a session or a project row: the same dialog (Finder cannot be told where
    /// to start).
    pub(crate) fn open_file_entry() -> MenuEntry {
        Self::entry("Open File…", |this, window, cx| {
            this.open_file_dialog(window, cx);
        })
    }

    /// Entries of a file tab's menu: the same file in a new split (one buffer, two views), and
    /// its path.
    pub(crate) fn file_tab_entries(&self, pane: PaneId, path: &str) -> Vec<MenuEntry> {
        let split = |label: &'static str, edge: Edge, path: String| {
            Self::entry(label, move |this, window, cx| {
                let item = ItemRef::file(path.clone());
                this.try_mutate(cx, |s| s.split_pane(pane, edge, item));
                this.focus_root(window, cx);
            })
        };
        let copy = path.to_owned();
        vec![
            split("Open in Split Right", Edge::Right, path.to_owned()),
            split("Open in Split Down", Edge::Bottom, path.to_owned()),
            Self::entry("Copy Path", move |this, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                this.set_notice("Copied the file's path", cx);
            }),
        ]
    }
}
