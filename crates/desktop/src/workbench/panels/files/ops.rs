//! The file operations of the Files tree (EX-7): Trash, duplicate, copy / cut / paste (the tree's
//! own clipboard and the files Finder copied), moves, and the session's undo journal.
//!
//! Every operation is a request to the daemon (`fs.trash`, `fs.copy`, `fs.move`, `fs.restore`,
//! `docs/protocol.md` § File operations): the panel asks, the daemon decides what is allowed and
//! never overwrites. What the panel adds is what a person expects around that: names for copies
//! (`a copy.txt`), one journal step per batch, the tree and the open tabs following what
//! happened (the workbench does the tabs: `file_ops.rs`), and a word when something could not be
//! done.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, ClipboardEntry, ClipboardItem, Context, Window};
use hutan_client::{ClientError, Connection};
use hutan_protocol::fs::{
    FsCopy, FsCopyItem, FsKind, FsMove, FsMoveItem, FsOpItem, FsOpResults, FsRestore, FsTrash,
    FsTrashed, OPS_ITEMS_MAX,
};
use hutan_protocol::{ErrorBody, ErrorCode};

use super::super::files_names::{COPY_TRIES, Step, copy_name, pointless_move};
use super::FilesPanel;
use super::edit::EditKind;
use crate::settings::FilesPanelSettings;
use crate::workbench::actions::{
    ChosenFile, FilesAskTrash, FilesTabsMoved, FilesTabsRestored, FilesTabsTrashed, Notify,
    OpenChosenFile,
};
use crate::workbench::files::FilePlace;

/// What the workbench's `Files: …` commands and the keys ask of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesCommand {
    NewFile,
    NewFolder,
    Rename,
    Duplicate,
    Trash,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    /// Put the keyboard in the filter field.
    Filter,
    /// Filter the folder of the selected row (the root when none is selected).
    FilterInFolder,
}

/// What ⌘C and ⌘X remembered.
#[derive(Debug, Clone)]
pub(super) struct PathClipboard {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
    /// The text put on the system clipboard with it: while the system clipboard still holds that
    /// text the tree's clipboard is what a paste means.
    pub text: String,
}

/// One thing to copy, and where its copy goes.
struct CopySource {
    from: PathBuf,
    /// The folder the copy goes into.
    dir: PathBuf,
    /// The source is something the user picked (Finder): it may lie outside the roots.
    picked: bool,
    /// The first candidate name to try: `0` the name itself, `1` `name copy`.
    start: usize,
}

/// What became of one copy.
enum CopyOutcome {
    Made(PathBuf),
    Failed(String),
}

/// The reason of an error answered by the daemon (`data.reason`).
fn reason_of(body: &ErrorBody) -> Option<&str> {
    body.data
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(serde_json::Value::as_str)
}

fn is_exists(body: &ErrorBody) -> bool {
    body.code == ErrorCode::Conflict && reason_of(body) == Some("exists")
}

/// What to tell about a refused operation on `name`.
pub(super) fn body_message(body: &ErrorBody, name: Option<&str>) -> String {
    let shown = name.map_or_else(String::new, |n| format!("“{n}” "));
    match (body.code, reason_of(body)) {
        (ErrorCode::Conflict, Some("exists")) => format!("{shown}already exists there").trim_start().to_owned(),
        (ErrorCode::NotFound, _) => format!("{shown}or its folder is not there any more").trim_start().to_owned(),
        (ErrorCode::Unsupported, Some("no_trash")) => {
            "This volume has no Trash: nothing was deleted".to_owned()
        }
        (ErrorCode::Unsupported, Some("cross_root")) => {
            "A file cannot move to another project root: copy it, then move the original to the Trash"
                .to_owned()
        }
        _ => body.message.clone(),
    }
}

/// [`body_message`] for an error of a single request.
pub(super) fn error_message(error: &ClientError, name: Option<&str>) -> String {
    match error {
        ClientError::Remote(body) => body_message(body, name.map(str::trim)),
        other => other.to_string(),
    }
}

/// `path` as it is after `from` became `to`: `to` itself, or what is below `from` moved below
/// `to`.
pub(super) fn remap(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_owned()
    } else {
        to.join(rest)
    })
}

/// A sentence about the first failures of a batch.
fn failures_sentence(verb: &str, failed: &[(String, String)]) -> String {
    match failed {
        [(name, why)] => format!("Cannot {verb} {name}: {why}"),
        [(name, why), rest @ ..] => {
            format!("Cannot {verb} {name}: {why} (and {} more)", rest.len())
        }
        [] => String::new(),
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl FilesPanel {
    /// Runs one of the `Files: …` commands.
    pub fn run_command(
        &mut self,
        command: FilesCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What makes a row (a new entry, a copy) needs the tree itself, not the filter's rows.
        if matches!(
            command,
            FilesCommand::NewFile
                | FilesCommand::NewFolder
                | FilesCommand::Duplicate
                | FilesCommand::Paste
        ) {
            self.leave_filter(cx);
        }
        match command {
            FilesCommand::Filter => self.start_filter(None, window, cx),
            FilesCommand::FilterInFolder => {
                let folder = self.selected_folder();
                if let Some(folder) = folder {
                    self.filter_in_folder(&folder, window, cx);
                }
            }
            FilesCommand::NewFile => self.begin_new(false, window, cx),
            FilesCommand::NewFolder => self.begin_new(true, window, cx),
            FilesCommand::Rename => {
                self.begin_rename(window, cx);
            }
            FilesCommand::Duplicate => self.duplicate(window, cx),
            FilesCommand::Trash => self.trash_selected(window, cx),
            FilesCommand::Copy => self.copy_selected(false, cx),
            FilesCommand::Cut => self.copy_selected(true, cx),
            FilesCommand::Paste => self.paste(window, cx),
            FilesCommand::Undo => self.undo(window, cx),
            FilesCommand::Redo => self.redo(window, cx),
        }
    }

    // ---- plumbing -----------------------------------------------------------------------------

    /// Runs `work` away from the UI and `then` with its answer.
    pub(super) fn spawn_op<R: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl Future<Output = R> + Send + 'static,
        then: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let id = self.next_op;
        self.next_op += 1;
        let answer = cx.background_spawn(work);
        let task = cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            this.update_in(cx, |this, window, cx| {
                this.ops.remove(&id);
                this.ops_done += 1;
                then(this, answer, window, cx);
            })
            .ok();
        });
        self.ops.insert(id, task);
    }

    /// A line in the workbench's notice area.
    pub(super) fn tell(
        &self,
        text: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.dispatch_action(Box::new(Notify(text.into())), cx);
    }

    /// The paths the commands act on: the selected rows, as the user sees them (a folded chain
    /// is its last folder), a folder standing for what is selected inside it too. Empty without
    /// a selection.
    pub fn acted_paths(&self) -> Vec<PathBuf> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for selected in self.selection() {
            if tree.is_root(&selected) {
                continue;
            }
            // The rows shown are the filter's while it narrows the tree.
            match self.index_of(&selected).map(|i| &self.rows()[i]) {
                Some(row) if row.path == selected => paths.push(row.folder().to_owned()),
                Some(_) => paths.push(selected),
                None => {}
            }
        }
        // Paths sort with what is below a folder right after it, so one pass finds those a
        // selected folder already carries.
        let mut sorted = paths.clone();
        sorted.sort();
        let mut kept: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut last: Option<PathBuf> = None;
        for path in sorted {
            if last.as_ref().is_some_and(|dir| path.starts_with(dir)) {
                continue;
            }
            last = Some(path.clone());
            kept.insert(path);
        }
        paths.retain(|p| kept.contains(p));
        paths.dedup();
        paths
    }

    /// Opens `path` in the editor with the keyboard.
    pub(super) fn open_in_editor(&self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(
            Box::new(OpenChosenFile(ChosenFile {
                path: path.display().to_string(),
                line: None,
                column: None,
                place: FilePlace::Focused,
            })),
            cx,
        );
    }

    /// Lists `dir` again now, when the tree shows it open.
    pub(super) fn relist_now(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.tree.as_ref().is_some_and(|t| t.is_expanded(dir)) {
            self.list(dir, cx);
        }
    }

    /// Selects `path` (and, with `rename`, opens its name field) once its folder lists it.
    pub(super) fn follow_after_listing(&mut self, path: &Path, rename: bool) {
        self.follow = Some((path.to_owned(), rename));
    }

    /// Takes `follow` once the row it names exists: selects it and scrolls to it.
    pub(super) fn apply_follow(&mut self) {
        let Some((path, rename)) = self.follow.clone() else {
            return;
        };
        let Some(index) = self.tree.as_ref().and_then(|t| t.index_of(&path)) else {
            return;
        };
        self.follow = None;
        self.select_index(index);
        if rename {
            self.rename_when_listed = Some(path);
        }
    }

    /// The folder pasted files go into: the selected folder, the folder of the selected file,
    /// else the root.
    fn paste_folder(&self) -> Option<PathBuf> {
        self.new_entry_folder()
    }

    // ---- Trash -------------------------------------------------------------------------------

    /// ⌘⌫ and *Move to Trash*: the selected row.
    pub fn trash_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.acted_paths();
        self.request_trash(paths, window, cx);
    }

    /// Moves `paths` to the Trash after the sheet (`panels.files.confirm_delete`, on by default)
    /// has listed them, the tabs with unsaved changes and an agent working there.
    pub fn request_trash(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        if self.env.settings.get::<FilesPanelSettings>().confirm_delete {
            let paths = paths.iter().map(|p| p.display().to_string()).collect();
            window.dispatch_action(Box::new(FilesAskTrash(paths)), cx);
        } else {
            self.trash_confirmed(paths, window, cx);
        }
    }

    /// The sheet was confirmed (or none is asked): `fs.trash`.
    pub fn trash_confirmed(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = FsTrash { paths };
        self.spawn_op(
            window,
            cx,
            async move {
                let mut all = Vec::new();
                let mut ticket = None;
                // The daemon takes 200 at a time; one ticket per call, so a larger batch is
                // several steps. (No explorer selection comes near that.)
                for chunk in request.paths.chunks(OPS_ITEMS_MAX) {
                    match connection
                        .fs_trash(FsTrash {
                            paths: chunk.to_vec(),
                        })
                        .await
                    {
                        Ok(trashed) => {
                            ticket = ticket.or(trashed.ticket);
                            all.extend(trashed.items);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(FsTrashed { ticket, items: all })
            },
            |this, answer, window, cx| match answer {
                Ok(trashed) => this.trashed(trashed, true, window, cx),
                Err(error) => {
                    let why = error_message(&error, None);
                    this.tell(format!("Cannot move to the Trash: {why}"), window, cx);
                }
            },
        );
    }

    /// What the Trash took: the tree and the tabs follow, a step is recorded (`record`), and the
    /// ones it did not take are told.
    fn trashed(
        &mut self,
        answer: FsTrashed,
        record: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut done = Vec::new();
        let mut failed = Vec::new();
        for item in answer.items {
            match (item.entry, item.error) {
                (Some(_), _) => done.push(item.path),
                (None, Some(body)) => {
                    failed.push((file_name_of(&item.path), body_message(&body, None)))
                }
                (None, None) => {}
            }
        }
        if !failed.is_empty() {
            self.tell(failures_sentence("move to the Trash", &failed), window, cx);
        }
        if done.is_empty() {
            return;
        }
        if record && let Some(ticket) = answer.ticket {
            self.journal.record(Step::Presence {
                paths: done.clone(),
                ticket: Some(ticket),
            });
        }
        self.gone_from_tree(&done, cx);
        window.dispatch_action(
            Box::new(FilesTabsTrashed(
                done.iter().map(|p| p.display().to_string()).collect(),
            )),
            cx,
        );
    }

    /// The tree after `paths` left: their rows go (the listings of their folders are read
    /// again) and the selection goes to the row that took the place of the first.
    fn gone_from_tree(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let first = paths.first().cloned();
        let index = first
            .as_ref()
            .and_then(|p| self.tree.as_ref().and_then(|t| t.index_of(p)));
        for path in paths {
            if let Some(tree) = &mut self.tree {
                for dir in tree.forget(path) {
                    self.unwatch(&dir, cx);
                }
            }
        }
        if let (Some(first), Some(index)) = (first, index)
            && let Some(parent) = first.parent()
        {
            self.hint = Some((parent.to_owned(), index));
        }
        let mut parents: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| p.parent().map(Path::to_owned))
            .collect();
        parents.sort();
        parents.dedup();
        for parent in parents {
            self.relist_now(&parent, cx);
        }
        self.keep_selection();
        cx.notify();
    }

    // ---- copy, cut, paste, duplicate ----------------------------------------------------------

    /// ⌘C / ⌘X: the selected row is remembered (and its path is put on the system clipboard as
    /// text, so a paste elsewhere has something and an older Finder copy is not mistaken for it).
    pub fn copy_selected(&mut self, cut: bool, cx: &mut Context<Self>) {
        let paths = self.acted_paths();
        if paths.is_empty() {
            return;
        }
        let text = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.clipboard = Some(PathClipboard { paths, cut, text });
        cx.notify();
    }

    /// The paths ⌘X left dimmed in the tree.
    pub fn is_cut(&self, path: &Path) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(|c| c.cut && c.paths.iter().any(|p| p == path))
    }

    /// What a ⌘V would paste, if the clipboard still says so.
    pub fn clipboard_paths(&self) -> Vec<PathBuf> {
        self.clipboard
            .as_ref()
            .map(|c| c.paths.clone())
            .unwrap_or_default()
    }

    /// ⌘V: the files Finder copied (copied in, wherever they are), else what ⌘C or ⌘X
    /// remembered, into the selected folder (the folder of the selected file; the root).
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.paste_folder() else {
            return;
        };
        let item = cx.read_from_clipboard();
        let external: Vec<PathBuf> = item
            .iter()
            .flat_map(|item| item.entries())
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !external.is_empty() {
            let sources = external
                .into_iter()
                .map(|from| CopySource {
                    from,
                    dir: folder.clone(),
                    picked: true,
                    start: 0,
                })
                .collect();
            self.reveal_folder(&folder, cx);
            self.run_copies(sources, false, window, cx);
            return;
        }
        let text = item.and_then(|i| i.text());
        let Some(clip) = self
            .clipboard
            .clone()
            .filter(|c| text.as_deref() == Some(c.text.as_str()))
        else {
            self.tell(
                "Nothing to paste: copy or cut a row of the tree, or copy files in Finder",
                window,
                cx,
            );
            return;
        };
        self.reveal_folder(&folder, cx);
        if clip.cut {
            self.clipboard = None;
            self.move_into(clip.paths, folder, window, cx);
        } else {
            let sources = clip
                .paths
                .into_iter()
                .map(|from| CopySource {
                    from,
                    dir: folder.clone(),
                    picked: false,
                    start: 0,
                })
                .collect();
            self.run_copies(sources, false, window, cx);
        }
    }

    /// Opens `folder` so what lands in it is seen.
    fn reveal_folder(&mut self, folder: &Path, cx: &mut Context<Self>) {
        let closed = self.tree.as_ref().is_some_and(|t| !t.is_expanded(folder));
        if closed {
            self.open_dir(folder, cx);
        }
    }

    /// ⌘D: the selected row copied next to itself as `name copy`; the name field opens on the copy.
    pub fn duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sources: Vec<CopySource> = self
            .acted_paths()
            .into_iter()
            .filter_map(|from| {
                let dir = from.parent()?.to_owned();
                Some(CopySource {
                    from,
                    dir,
                    picked: false,
                    start: 1,
                })
            })
            .collect();
        if sources.is_empty() {
            return;
        }
        self.run_copies(sources, true, window, cx);
    }

    /// Copies `sources`, each under the first free name (`a.txt`, `a copy.txt`, `a copy 2.txt`);
    /// one journal step for the batch. With `rename_one`, a single copy opens its name field.
    fn run_copies(
        &mut self,
        sources: Vec<CopySource>,
        rename_one: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.env.sessions.read(cx).connection().clone();
        self.spawn_op(
            window,
            cx,
            copy_all(connection, sources),
            move |this, outcomes, window, cx| this.copied(outcomes, rename_one, window, cx),
        );
    }

    fn copied(
        &mut self,
        outcomes: Vec<(PathBuf, CopyOutcome)>,
        rename_one: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut made = Vec::new();
        let mut failed = Vec::new();
        for (from, outcome) in outcomes {
            match outcome {
                CopyOutcome::Made(to) => made.push(to),
                CopyOutcome::Failed(why) => failed.push((file_name_of(&from), why)),
            }
        }
        if !failed.is_empty() {
            self.tell(failures_sentence("copy", &failed), window, cx);
        }
        if made.is_empty() {
            return;
        }
        self.journal.record(Step::Presence {
            paths: made.clone(),
            ticket: None,
        });
        let mut parents: Vec<PathBuf> = made
            .iter()
            .filter_map(|p| p.parent().map(Path::to_owned))
            .collect();
        parents.sort();
        parents.dedup();
        for parent in parents {
            self.relist_now(&parent, cx);
        }
        if let [only] = made.as_slice() {
            self.follow_after_listing(only, rename_one);
        } else if let Some(first) = made.first() {
            self.follow_after_listing(first, false);
        }
        cx.notify();
    }

    // ---- moves ---------------------------------------------------------------------------------

    /// Moves `items` into the folder `dir` (a drag, a paste of what ⌘X remembered). What is
    /// there already, or is the folder itself, is left alone; a name that is taken in `dir` is
    /// told, never overwritten.
    pub fn move_into(
        &mut self,
        items: Vec<PathBuf>,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let moves: Vec<FsMoveItem> = items
            .into_iter()
            .filter(|from| !pointless_move(from, &dir))
            .filter_map(|from| {
                let to = dir.join(from.file_name()?);
                Some(FsMoveItem { from, to })
            })
            .collect();
        if moves.is_empty() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let pairs: Vec<(PathBuf, PathBuf)> = moves
            .iter()
            .map(|m| (m.from.clone(), m.to.clone()))
            .collect();
        self.spawn_op(
            window,
            cx,
            async move { move_all(connection, moves).await },
            move |this, answer, window, cx| match answer {
                Ok(results) => {
                    let (done, failed) = split_results(&pairs, results);
                    if !failed.is_empty() {
                        this.tell(failures_sentence("move", &failed), window, cx);
                    }
                    this.record_moved(done, window, cx);
                }
                Err(error) => {
                    let why = error_message(&error, None);
                    this.tell(format!("Cannot move: {why}"), window, cx);
                }
            },
        );
    }

    /// Copies `items` into the folder `dir` (⌥-drag): each under the first free name.
    pub fn copy_into(
        &mut self,
        items: Vec<PathBuf>,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sources = items
            .into_iter()
            .map(|from| CopySource {
                from,
                dir: dir.clone(),
                picked: false,
                start: 0,
            })
            .collect();
        self.run_copies(sources, false, window, cx);
    }

    /// The moves happened: one journal step, the tree and the tabs follow.
    pub(super) fn record_moved(
        &mut self,
        pairs: Vec<(PathBuf, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pairs.is_empty() {
            return;
        }
        self.journal.record(Step::Moved(pairs.clone()));
        self.moved_on_disk(&pairs, window, cx);
    }

    /// The tree and the tabs after `pairs` (`(from, to)`) were renamed or moved on disk: open
    /// folders stay open under their new names, the selection follows, the folders on both
    /// sides are listed again, and the workbench re-keys the tabs, their text and the language
    /// server's documents.
    pub(super) fn moved_on_disk(
        &mut self,
        pairs: &[(PathBuf, PathBuf)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut reopen: Vec<PathBuf> = Vec::new();
        if let Some(tree) = &self.tree {
            for dir in tree.expanded_dirs() {
                for (from, to) in pairs {
                    if let Some(new) = remap(&dir, from, to)
                        && !tree.is_root(&dir)
                    {
                        reopen.push(new);
                        break;
                    }
                }
            }
        }
        for (from, _) in pairs {
            if let Some(tree) = &mut self.tree {
                for dir in tree.forget(from) {
                    self.unwatch(&dir, cx);
                }
            }
        }
        if let Some(selected) = self.selected.clone() {
            for (from, to) in pairs {
                if let Some(new) = remap(&selected, from, to) {
                    self.selected = Some(new);
                    break;
                }
            }
        }
        if !self.picked.is_empty() {
            let moved = std::mem::take(&mut self.picked);
            self.picked = moved
                .into_iter()
                .map(|p| {
                    pairs
                        .iter()
                        .find_map(|(from, to)| remap(&p, from, to))
                        .unwrap_or(p)
                })
                .collect();
        }
        reopen.sort_by_key(|d| d.components().count());
        for dir in reopen {
            if let Some(tree) = &mut self.tree {
                tree.expand(&dir);
            }
            self.list(&dir, cx);
            self.watch(&dir, cx);
        }
        let mut parents: Vec<PathBuf> = pairs
            .iter()
            .flat_map(|(from, to)| [from.parent(), to.parent()])
            .flatten()
            .map(Path::to_owned)
            .collect();
        parents.sort();
        parents.dedup();
        for parent in parents {
            self.relist_now(&parent, cx);
        }
        if let Some((_, to)) = pairs.first() {
            self.follow_after_listing(to, false);
        }
        window.dispatch_action(
            Box::new(FilesTabsMoved(
                pairs
                    .iter()
                    .map(|(from, to)| (from.display().to_string(), to.display().to_string()))
                    .collect(),
            )),
            cx,
        );
        self.after_tree_change(cx);
        cx.notify();
    }

    // ---- undo and redo ------------------------------------------------------------------------

    /// ⌘Z: takes back the last operation of this window's tree (a batch is one step).
    pub fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.journal.pop_undo() {
            Some(step) => self.flip(step, true, window, cx),
            None => self.tell("Nothing to undo in the Files tree", window, cx),
        }
    }

    /// ⇧⌘Z: does again what the last undo took back.
    pub fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.journal.pop_redo() {
            Some(step) => self.flip(step, false, window, cx),
            None => self.tell("Nothing to redo in the Files tree", window, cx),
        }
    }

    /// Operations that have been undone and could be done again, and those that could be undone.
    pub fn journal_depth(&self) -> (usize, usize) {
        (self.journal.undoable(), self.journal.redoable())
    }

    /// Puts `step` back where it came from (it could not be flipped, all or in part), or on the
    /// other stack (it was).
    fn settle(&mut self, step: Option<Step>, undoing: bool, flipped: bool) {
        let Some(step) = step else {
            return;
        };
        match (undoing, flipped) {
            (true, false) | (false, true) => self.journal.push_undo(step),
            (true, true) | (false, false) => self.journal.push_redo(step),
        }
    }

    /// Flips `step`: what was moved goes back (or forth again), what was made goes to the Trash,
    /// what went to the Trash comes back. `undoing`: the step came off the undo stack.
    fn flip(&mut self, step: Step, undoing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.env.sessions.read(cx).connection().clone();
        match step {
            Step::Moved(pairs) => {
                let moves: Vec<FsMoveItem> = pairs
                    .iter()
                    .map(|(from, to)| FsMoveItem {
                        from: to.clone(),
                        to: from.clone(),
                    })
                    .collect();
                self.spawn_op(
                    window,
                    cx,
                    async move { move_all(connection, moves).await },
                    move |this, answer, window, cx| match answer {
                        Ok(results) => {
                            let back: Vec<(PathBuf, PathBuf)> = pairs
                                .iter()
                                .map(|(from, to)| (to.clone(), from.clone()))
                                .collect();
                            let (done, failed) = split_results(&back, results);
                            if !failed.is_empty() {
                                this.tell(failures_sentence("move", &failed), window, cx);
                            }
                            // The pairs that did not move stay as they were, on the stack
                            // they came from.
                            let stuck: Vec<(PathBuf, PathBuf)> = pairs
                                .into_iter()
                                .filter(|(from, to)| {
                                    !done.iter().any(|(was, now)| was == to && now == from)
                                })
                                .collect();
                            this.settle(
                                (!stuck.is_empty()).then_some(Step::Moved(stuck)),
                                undoing,
                                false,
                            );
                            if !done.is_empty() {
                                this.settle(Some(Step::Moved(done.clone())), undoing, true);
                                this.moved_on_disk(&done, window, cx);
                            }
                        }
                        Err(error) => {
                            this.settle(Some(Step::Moved(pairs)), undoing, false);
                            let why = error_message(&error, None);
                            this.tell(format!("Cannot move: {why}"), window, cx);
                        }
                    },
                );
            }
            Step::Presence {
                paths,
                ticket: None,
            } => {
                let request = paths.clone();
                self.spawn_op(
                    window,
                    cx,
                    async move { connection.fs_trash(FsTrash { paths: request }).await },
                    move |this, answer, window, cx| match answer {
                        Ok(trashed) => {
                            let ticket = trashed.ticket.clone();
                            let done: Vec<PathBuf> = trashed
                                .items
                                .iter()
                                .filter(|i| i.entry.is_some())
                                .map(|i| i.path.clone())
                                .collect();
                            let stuck: Vec<PathBuf> =
                                paths.into_iter().filter(|p| !done.contains(p)).collect();
                            this.settle(
                                (!stuck.is_empty()).then_some(Step::Presence {
                                    paths: stuck,
                                    ticket: None,
                                }),
                                undoing,
                                false,
                            );
                            if let Some(ticket) = ticket
                                && !done.is_empty()
                            {
                                this.settle(
                                    Some(Step::Presence {
                                        paths: done.clone(),
                                        ticket: Some(ticket),
                                    }),
                                    undoing,
                                    true,
                                );
                            }
                            this.trashed(trashed, false, window, cx);
                        }
                        Err(error) => {
                            this.settle(
                                Some(Step::Presence {
                                    paths,
                                    ticket: None,
                                }),
                                undoing,
                                false,
                            );
                            let why = error_message(&error, None);
                            this.tell(format!("Cannot move to the Trash: {why}"), window, cx);
                        }
                    },
                );
            }
            Step::Presence {
                paths,
                ticket: Some(ticket),
            } => {
                let request = FsRestore {
                    ticket: ticket.clone(),
                };
                self.spawn_op(
                    window,
                    cx,
                    async move { connection.fs_restore(request).await },
                    move |this, answer, window, cx| match answer {
                        Ok(results) => {
                            let (done, failed) = split_op_items(results);
                            if !failed.is_empty() {
                                this.tell(failures_sentence("restore", &failed), window, cx);
                            }
                            let stuck: Vec<PathBuf> =
                                paths.into_iter().filter(|p| !done.contains(p)).collect();
                            this.settle(
                                (!stuck.is_empty()).then_some(Step::Presence {
                                    paths: stuck,
                                    ticket: Some(ticket),
                                }),
                                undoing,
                                false,
                            );
                            if !done.is_empty() {
                                this.settle(
                                    Some(Step::Presence {
                                        paths: done.clone(),
                                        ticket: None,
                                    }),
                                    undoing,
                                    true,
                                );
                                this.back_in_tree(&done, window, cx);
                            }
                        }
                        Err(error) => {
                            this.settle(
                                Some(Step::Presence {
                                    paths,
                                    ticket: Some(ticket),
                                }),
                                undoing,
                                false,
                            );
                            let why = error_message(&error, None);
                            this.tell(format!("Cannot restore from the Trash: {why}"), window, cx);
                        }
                    },
                );
            }
        }
    }

    /// `paths` came back from the Trash: their folders are listed again, the first is selected,
    /// and the tabs that said they were deleted find them.
    fn back_in_tree(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let mut parents: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| p.parent().map(Path::to_owned))
            .collect();
        parents.sort();
        parents.dedup();
        for parent in parents {
            self.relist_now(&parent, cx);
        }
        if let Some(first) = paths.first() {
            self.follow_after_listing(first, false);
        }
        window.dispatch_action(
            Box::new(FilesTabsRestored(
                paths.iter().map(|p| p.display().to_string()).collect(),
            )),
            cx,
        );
        cx.notify();
    }

    /// Whether the field being edited makes something new (tests).
    pub fn is_making(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|e| e.kind != EditKind::Rename)
    }

    /// Operations whose answer has not arrived (tests wait for none).
    pub fn ops_running(&self) -> usize {
        self.ops.len()
    }

    /// Operations answered so far.
    pub fn ops_done(&self) -> usize {
        self.ops_done
    }
}

/// `(from, to)` of each thing moved.
type Moved = Vec<(PathBuf, PathBuf)>;

/// `(name, reason)` of each item of a batch that was refused.
type Refused = Vec<(String, String)>;

/// Splits what `fs.move` answered into the pairs that moved and those that did not.
fn split_results(pairs: &[(PathBuf, PathBuf)], results: FsOpResults) -> (Moved, Refused) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for ((from, to), item) in pairs.iter().zip(results.items) {
        match item.error {
            None if item.entry.is_some() => done.push((from.clone(), to.clone())),
            Some(body) => failed.push((
                file_name_of(from),
                body_message(
                    &body,
                    to.file_name().map(|n| n.to_string_lossy()).as_deref(),
                ),
            )),
            None => {}
        }
    }
    (done, failed)
}

/// The paths of the items that went well, and `(name, reason)` of those that did not.
fn split_op_items(results: FsOpResults) -> (Vec<PathBuf>, Vec<(String, String)>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for item in results.items {
        let FsOpItem { path, entry, error } = item;
        match (entry, error) {
            (Some(_), _) => done.push(path),
            (None, Some(body)) => failed.push((file_name_of(&path), body_message(&body, None))),
            (None, None) => {}
        }
    }
    (done, failed)
}

/// `fs.move` in batches the daemon takes.
async fn move_all(
    connection: Connection,
    moves: Vec<FsMoveItem>,
) -> Result<FsOpResults, ClientError> {
    let mut items = Vec::with_capacity(moves.len());
    for chunk in moves.chunks(OPS_ITEMS_MAX) {
        let answer = connection
            .fs_move(FsMove {
                items: chunk.to_vec(),
            })
            .await?;
        items.extend(answer.items);
    }
    Ok(FsOpResults { items })
}

/// Copies every source, a name that is taken moving it on to the next (`a copy.txt`, `a copy 2.txt`):
/// the daemon never overwrites, so a conflict is the signal to try the next name.
async fn copy_all(connection: Connection, sources: Vec<CopySource>) -> Vec<(PathBuf, CopyOutcome)> {
    let count = sources.len();
    let mut outcomes: Vec<Option<CopyOutcome>> = (0..count).map(|_| None).collect();
    let mut attempt: Vec<usize> = sources.iter().map(|s| s.start).collect();
    let mut is_dir: Vec<Option<bool>> = vec![None; count];
    let mut pending: Vec<usize> = (0..count).collect();
    while !pending.is_empty() {
        let mut items = Vec::with_capacity(pending.len());
        for &i in &pending {
            let source = &sources[i];
            let name = file_name_of(&source.from);
            if attempt[i] > 0 && is_dir[i].is_none() {
                is_dir[i] = Some(
                    connection
                        .fs_stat(&source.from, source.picked)
                        .await
                        .is_ok_and(|e| e.kind == FsKind::Dir),
                );
            }
            let name = copy_name(&name, is_dir[i].unwrap_or(false), attempt[i]);
            items.push(FsCopyItem {
                from: source.from.clone(),
                to: source.dir.join(name),
                picked_from: source.picked,
            });
        }
        let mut next = Vec::new();
        for (chunk, indices) in items
            .chunks(OPS_ITEMS_MAX)
            .zip(pending.chunks(OPS_ITEMS_MAX))
        {
            let answer = connection
                .fs_copy(FsCopy {
                    items: chunk.to_vec(),
                })
                .await;
            match answer {
                Ok(results) => {
                    for ((item, &i), request) in results.items.into_iter().zip(indices).zip(chunk) {
                        match (item.entry, item.error) {
                            (Some(_), _) => {
                                outcomes[i] = Some(CopyOutcome::Made(request.to.clone()));
                            }
                            (None, Some(body))
                                if is_exists(&body)
                                    && attempt[i] + 1 < sources[i].start + COPY_TRIES =>
                            {
                                attempt[i] += 1;
                                next.push(i);
                            }
                            (None, Some(body)) => {
                                outcomes[i] = Some(CopyOutcome::Failed(body_message(
                                    &body,
                                    request
                                        .to
                                        .file_name()
                                        .map(|n| n.to_string_lossy())
                                        .as_deref(),
                                )));
                            }
                            (None, None) => {}
                        }
                    }
                }
                Err(error) => {
                    let why = error_message(&error, None);
                    for &i in indices {
                        outcomes[i] = Some(CopyOutcome::Failed(why.clone()));
                    }
                }
            }
        }
        pending = next;
    }
    sources
        .into_iter()
        .zip(outcomes)
        .filter_map(|(s, o)| o.map(|o| (s.from, o)))
        .collect()
}
