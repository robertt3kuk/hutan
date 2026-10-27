//! The inline name editor of the Files tree (EX-7): a new file or folder, and a rename.
//!
//! A new entry is a row of its own, first inside its folder, with the name field in it; a rename
//! turns the name of the row into the field with the stem selected (`main` of `main.rs`). Return
//! asks the daemon (`fs.create`, `fs.rename`), Escape drops the edit, and so does moving the
//! keyboard elsewhere when the name is not one to make (it makes the one that is, the way Finder
//! does). A name that cannot be used stays in the field with the reason under the tree: the
//! daemon never overwrites, so a name that is taken is told here (and again by the daemon when
//! the listing was stale).

use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    SharedString, Styled, Subscription, Window, div, px,
};
use hutan_protocol::fs::{FsCreate, FsCreateKind, FsRename};

use super::super::files_names::{NameProblem, check_name, collision, stem_range};
use super::super::files_tree::RowKind;
use super::{FilesPanel, error_message};
use crate::theme::DensityExt as _;
use crate::workbench::chat::vim::Outcome;
use crate::workbench::style::UiStyle;
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::FocusRingExt as _;
use crate::workbench::vim_ui::{self, VimConfig};

/// What the name field makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    NewFile,
    NewFolder,
    Rename,
}

/// The name being typed.
pub(super) struct NameEdit {
    pub kind: EditKind,
    /// The folder a new entry goes in; for a rename, the folder that holds the entry.
    pub parent: PathBuf,
    /// The entry being renamed.
    pub target: Option<PathBuf>,
    /// The entry being renamed is a folder (its whole name is selected, and drawn as one).
    pub is_dir: bool,
    pub text: String,
    pub error: Option<String>,
    /// The request is on its way: the field takes no more commits.
    pub busy: bool,
    pub _blur: Subscription,
}

impl NameEdit {
    /// The entry the field names is a folder.
    pub fn is_dir_entry(&self) -> bool {
        match self.kind {
            EditKind::NewFolder => true,
            EditKind::NewFile => false,
            EditKind::Rename => self.is_dir,
        }
    }
}

/// What a test reads of the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEditView {
    pub kind: EditKind,
    pub parent: PathBuf,
    pub target: Option<PathBuf>,
    pub text: String,
    pub error: Option<String>,
    /// The selected bytes of `text`.
    pub selection: Option<Range<usize>>,
    pub busy: bool,
}

impl FilesPanel {
    /// The name field, when one is open.
    pub fn editing(&self) -> Option<NameEditView> {
        let edit = self.editing.as_ref()?;
        Some(NameEditView {
            kind: edit.kind,
            parent: edit.parent.clone(),
            target: edit.target.clone(),
            text: edit.text.clone(),
            error: edit.error.clone(),
            selection: self.name_edit.selection(&edit.text),
            busy: edit.busy,
        })
    }

    /// The folder *New File* and *New Folder* put the entry in: the selected folder, the folder
    /// of the selected file, else the root.
    pub(super) fn new_entry_folder(&self) -> Option<PathBuf> {
        let tree = self.tree.as_ref()?;
        let Some(selected) = self.selected.clone() else {
            return Some(tree.root().to_owned());
        };
        match tree.index_of(&selected).map(|i| &tree.rows()[i]) {
            Some(row) if row.kind == RowKind::Dir => Some(if row.path == selected {
                row.folder().to_owned()
            } else {
                selected
            }),
            Some(_) => selected.parent().map(Path::to_owned),
            None => Some(tree.root().to_owned()),
        }
    }

    /// *New File* / *New Folder* (the header's buttons, ⌘N / ⌥⌘N, the row menu): opens a name
    /// field first inside the selected folder, which is opened (and listed) for it.
    pub fn begin_new(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.new_entry_folder() else {
            return;
        };
        self.drop_edit();
        let opened = self.tree.as_ref().is_some_and(|t| t.is_expanded(&parent));
        if !opened {
            self.open_dir(&parent, cx);
        }
        let kind = if folder {
            EditKind::NewFolder
        } else {
            EditKind::NewFile
        };
        self.start_edit(
            NameEditStart {
                kind,
                parent,
                target: None,
                is_dir: folder,
                text: String::new(),
            },
            0..0,
            window,
            cx,
        );
        if let Some((slot, _)) = self.new_row_slot() {
            self.scroll
                .scroll_to_item(slot, gpui::ScrollStrategy::Nearest);
        }
    }

    /// F2 and *Rename*: the name of the selected row becomes the field, its stem selected.
    /// `false` when nothing is selected or several rows are (a rename is of one name).
    pub fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.selection().len() > 1 {
            return false;
        }
        let Some(target) = self.acted_paths().into_iter().next() else {
            return false;
        };
        self.begin_rename_of(target, window, cx)
    }

    /// [`Self::begin_rename`] for `target`.
    pub fn begin_rename_of(
        &mut self,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(name), Some(parent)) = (
            target.file_name().map(|n| n.to_string_lossy().into_owned()),
            target.parent().map(Path::to_owned),
        ) else {
            return false;
        };
        let is_dir = self
            .tree
            .as_ref()
            .and_then(|t| t.kind_of(&target))
            .is_some_and(|k| k == hutan_protocol::fs::FsKind::Dir);
        self.drop_edit();
        let selection = stem_range(&name, is_dir);
        self.set_single(target.clone());
        if let Some(index) = self.tree.as_ref().and_then(|t| t.index_of(&target)) {
            self.scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        }
        self.start_edit(
            NameEditStart {
                kind: EditKind::Rename,
                parent,
                target: Some(target),
                is_dir,
                text: name,
            },
            selection,
            window,
            cx,
        );
        true
    }

    fn start_edit(
        &mut self,
        start: NameEditStart,
        selection: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let blur = cx.on_focus_out(&self.name_focus, window, |this, _, window, cx| {
            this.blurred(window, cx);
        });
        self.name_edit.reset_selecting(&start.text, selection);
        self.editing = Some(NameEdit {
            kind: start.kind,
            parent: start.parent,
            target: start.target,
            is_dir: start.is_dir,
            text: start.text,
            error: None,
            busy: false,
            _blur: blur,
        });
        cx.defer_in(window, |this, window, cx| {
            if this.editing.is_some() {
                window.focus(&this.name_focus, cx);
            }
        });
        cx.notify();
    }

    /// Forgets the field without giving the keyboard anywhere.
    pub(super) fn drop_edit(&mut self) {
        self.editing = None;
    }

    /// Escape: the field goes, nothing is made or renamed, the keyboard is back in the tree.
    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// The keyboard left the field: a name that can be made is made, any other edit is dropped.
    fn blurred(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.editing else {
            return;
        };
        if edit.busy {
            return;
        }
        let unchanged = edit.kind == EditKind::Rename
            && edit
                .target
                .as_ref()
                .and_then(|t| t.file_name())
                .map(|n| n.to_string_lossy())
                == Some(edit.text.trim().into());
        if unchanged || self.problem_with(&edit.text.clone()).is_some() {
            self.editing = None;
            cx.notify();
            return;
        }
        self.commit_edit(window, cx);
    }

    /// What is wrong with `text` as a name here, if anything: its shape, then the neighbours.
    fn problem_with(&self, text: &str) -> Option<NameProblem> {
        let edit = self.editing.as_ref()?;
        let name = match check_name(text) {
            Ok(name) => name,
            Err(problem) => return Some(problem),
        };
        let names = self.tree.as_ref()?.entry_names(&edit.parent)?;
        let own = edit
            .target
            .as_ref()
            .and_then(|t| t.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        collision(names.iter().copied(), name, own.as_deref())
            .map(|existing| NameProblem::Exists(existing.to_owned()))
    }

    /// Return: asks the daemon to make (or rename to) the typed name. A name that cannot be used
    /// stays in the field with the reason under the tree.
    pub fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.editing else {
            return;
        };
        if edit.busy {
            return;
        }
        let (kind, parent, target, text) = (
            edit.kind,
            edit.parent.clone(),
            edit.target.clone(),
            edit.text.clone(),
        );
        if let Some(problem) = self.problem_with(&text) {
            if let Some(edit) = &mut self.editing {
                edit.error = Some(problem.message());
            }
            cx.notify();
            return;
        }
        let name = text.trim().to_owned();
        let path = parent.join(&name);
        match kind {
            EditKind::NewFile | EditKind::NewFolder => {
                self.run_create(path, kind == EditKind::NewFolder, window, cx);
            }
            EditKind::Rename => {
                let Some(from) = target else {
                    return;
                };
                if from == path {
                    self.cancel_edit(window, cx);
                    return;
                }
                self.run_rename(from, path, window, cx);
            }
        }
    }

    fn run_create(
        &mut self,
        path: PathBuf,
        dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(edit) = &mut self.editing {
            edit.busy = true;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = FsCreate {
            path: path.clone(),
            kind: if dir {
                FsCreateKind::Dir
            } else {
                FsCreateKind::File
            },
            parents: false,
        };
        self.spawn_op(
            window,
            cx,
            async move { connection.fs_create(request).await },
            move |this, answer, window, cx| match answer {
                Ok(_) => this.created(path, dir, window, cx),
                Err(error) => this.edit_failed(&error, window, cx),
            },
        );
    }

    fn run_rename(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(edit) = &mut self.editing {
            edit.busy = true;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = FsRename {
            from: from.clone(),
            to: to.clone(),
        };
        self.spawn_op(
            window,
            cx,
            async move { connection.fs_rename(request).await },
            move |this, answer, window, cx| match answer {
                Ok(_) => {
                    this.editing = None;
                    window.focus(&this.focus, cx);
                    this.record_moved(vec![(from, to)], window, cx);
                }
                Err(error) => this.edit_failed(&error, window, cx),
            },
        );
    }

    /// The daemon refused the name: it stays in the field, with the reason.
    fn edit_failed(
        &mut self,
        error: &hutan_client::ClientError,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(edit) = &mut self.editing {
            edit.busy = false;
            edit.error = Some(error_message(error, Some(&edit.text)));
        }
        cx.notify();
    }

    /// The new entry exists: the field goes, the entry is selected once its folder is listed
    /// again, and a file opens in a tab.
    fn created(&mut self, path: PathBuf, dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        window.focus(&self.focus, cx);
        self.journal
            .record(super::super::files_names::Step::Presence {
                paths: vec![path.clone()],
                ticket: None,
            });
        self.follow_after_listing(&path, false);
        if let Some(parent) = path.parent() {
            self.relist_now(parent, cx);
        }
        if !dir {
            self.open_in_editor(&path, window, cx);
        }
        cx.notify();
    }

    // ---- keys and drawing --------------------------------------------------------------------

    fn on_name_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        let Some(edit) = &mut self.editing else {
            return;
        };
        if edit.busy {
            cx.stop_propagation();
            return;
        }
        if self.name_edit.key(&mut edit.text, event, vim, true, cx) == Outcome::Handled {
            edit.error = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" => self.commit_edit(window, cx),
            "escape" => self.cancel_edit(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// How the field's element reaches its text.
    fn name_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.name_focus.clone(),
            |this: &mut FilesPanel, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(edit) = &mut this.editing {
                    let before = edit.text.clone();
                    this.name_edit.with(&mut edit.text, vim_on, false, f);
                    if edit.text != before {
                        edit.error = None;
                    }
                }
                cx.notify();
            },
        )
    }

    /// Replaces the typed name (tests, and the end of a paste that the field's own input did
    /// not take): the whole text is the caret's.
    pub fn set_name_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.editing {
            edit.text = text.to_owned();
            edit.error = None;
            cx.notify();
        }
    }

    /// Where the new entry's row goes: `(index among the rows drawn, depth)`; `None` while
    /// there is none to draw (no field, a rename, or its folder is not on screen).
    pub(super) fn new_row_slot(&self) -> Option<(usize, usize)> {
        let edit = self
            .editing
            .as_ref()
            .filter(|e| e.kind != EditKind::Rename)?;
        let tree = self.tree.as_ref()?;
        if edit.parent == tree.root() {
            return Some((0, 0));
        }
        let at = tree
            .rows()
            .iter()
            .position(|r| r.kind == RowKind::Dir && r.expanded && r.folder() == edit.parent)?;
        Some((at + 1, tree.rows()[at].depth + 1))
    }

    /// Drops a field whose row went from the screen (its folder was closed, the entry removed).
    pub(super) fn keep_edit_visible(&mut self) {
        let Some(edit) = &self.editing else {
            return;
        };
        let visible = match edit.kind {
            EditKind::Rename => edit.target.as_ref().is_some_and(|t| {
                self.tree
                    .as_ref()
                    .is_some_and(|tree| tree.index_of(t).is_some())
            }),
            _ => self.new_row_slot().is_some(),
        };
        if !visible {
            self.editing = None;
        }
    }

    /// The path of the entry a rename edits, when `path` is the row being renamed.
    pub(super) fn renaming(&self, path: &Path) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|e| e.kind == EditKind::Rename && e.target.as_deref() == Some(path))
    }

    /// The field itself: a box in the row, the typed name inside.
    pub(super) fn name_editor(&self, cx: &Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let text = self
            .editing
            .as_ref()
            .map_or_else(String::new, |e| e.text.clone());
        let error = self.editing.as_ref().is_some_and(|e| e.error.is_some());
        div()
            .id("files-name-edit")
            .key_context("FilesNameEdit")
            .track_focus(&self.name_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_name_key(event, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .h(px(tokens.sidebar_row - 8.0))
            .px(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .bg(theme.bg)
            .border_1()
            .border_color(if error { theme.danger } else { theme.border })
            .focus_ring(true, &theme, &tokens)
            .text_color(theme.fg)
            .children(vim_ui::string_field(
                SharedString::from("files-name-text"),
                &self.name_edit,
                &text,
                VimConfig::of(&self.env.settings).enabled,
                "",
                true,
                false,
                self.name_field(cx),
                &theme,
                &tokens,
            ))
            .into_any_element()
    }

    /// The reason the typed name was not taken, in a strip under the tree.
    pub(super) fn name_error_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let message = self.editing.as_ref()?.error.clone()?;
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        Some(
            div()
                .id("files-name-error")
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.danger)
                .child(SharedString::from(message))
                .into_any_element(),
        )
    }
}

/// What opens the field.
struct NameEditStart {
    kind: EditKind,
    parent: PathBuf,
    target: Option<PathBuf>,
    is_dir: bool,
    text: String,
}
