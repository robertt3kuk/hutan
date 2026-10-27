//! The Files tree's row menu and the commands behind it (EX-4): what a right click on a row
//! offers, grouped with lines and the key that does the same, and what each entry does with
//! the selected rows.
//!
//! Groups, top to bottom: open (Open, Open to the Side, Reveal in Finder), compare (Select for
//! Compare, Compare with Selected, Compare with HEAD; Compare Selected for two files), the
//! hooks (Open in Terminal, Attach to Chat, Find in Folder, Show File History, Collapse Folder),
//! the paths (Copy Path, Copy Relative Path) and the file operations of EX-7 (New File … Move to
//! Trash). A click on a row inside a selection of several is about all of them: the actions that
//! read (open, reveal, copy the path, attach) take every row, the ones with one subject (a
//! terminal, a search, a compare, a name) are left out; the file operations take every row except
//! New File, New Folder and Rename. Every entry is also a command of the palette
//! (`Files: …`), acting on the tree's selection.

use std::path::{Path, PathBuf};

use gpui::{Action, App, ClipboardItem, Context, KeyContext, SharedString, Window};
use hutan_protocol::fs::FsKind;

use super::actions::{self, FilesMenuAt};
use super::files::{FileOpen, FilePlace};
use super::model::ItemRef;
use super::panels::{FilesCommand, FilesPanel, SearchPanel};
use super::view::{MenuEntry, Workbench};

/// What a row-menu entry or a `Files: …` command does with the selected rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesRowAction {
    Open,
    OpenToSide,
    Reveal,
    CopyPath,
    CopyRelativePath,
    OpenInTerminal,
    AttachToChat,
    FilterInFolder,
    FindInFolder,
    ShowHistory,
    CollapseFolder,
    SelectForCompare,
    CompareWithSelected,
    CompareSelected,
    CompareWithHead,
}

/// The "⌘C"-style hint of the key that runs `action` while the keyboard is in the Files tree.
fn files_key_hint(window: &Window, action: &dyn Action) -> Option<SharedString> {
    let context = KeyContext::parse(actions::FILES_KEY_CONTEXT).ok()?;
    let binding = window.highest_precedence_binding_for_action_in_context(action, context)?;
    let text: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|k| capital_function_key(&k.to_string()))
        .collect();
    Some(text.join(" ").into())
}

/// `f2` as `F2` (GPUI spells a key of more than one letter as the binding does), after any
/// modifier symbols.
fn capital_function_key(text: &str) -> String {
    let key_at = text
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(text.len(), |(at, _)| at);
    let (modifiers, key) = text.split_at(key_at);
    match key.strip_prefix('f') {
        Some(digits) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            format!("{modifiers}F{digits}")
        }
        _ => text.to_owned(),
    }
}

/// `pattern` with the characters a glob reads specially taken literally.
fn glob_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}') {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

impl Workbench {
    // ---- what the commands act on ----------------------------------------------------------

    fn files_panel_entity(&self) -> Option<gpui::Entity<FilesPanel>> {
        self.panel::<FilesPanel>("files")
    }

    /// The rows the Files commands act on, and the tree's root.
    fn files_subject(&self, cx: &App) -> (Vec<PathBuf>, Option<PathBuf>) {
        match self.files_panel_entity() {
            Some(panel) => {
                let panel = panel.read(cx);
                (panel.acted_paths(), panel.root().map(Path::to_owned))
            }
            None => (Vec::new(), None),
        }
    }

    /// Whether the tree shows `path` as a folder (`None` when it does not know it).
    fn files_kind(&self, path: &Path, cx: &App) -> Option<FsKind> {
        let panel = self.files_panel_entity()?;
        panel.read(cx).tree()?.kind_of(path)
    }

    fn files_is_dir(&self, path: &Path, cx: &App) -> bool {
        self.files_kind(path, cx) == Some(FsKind::Dir)
    }

    /// Reveal in Finder needs the daemon's files on this Mac.
    fn files_can_reveal(&self, cx: &App) -> bool {
        !matches!(
            self.sessions.read(cx).connection().endpoint(),
            Some(hutan_client::Endpoint::Ssh(_))
        )
    }

    // ---- the menu --------------------------------------------------------------------------

    /// The context menu of a Files panel row. The row the click was on is part of the
    /// selection (the click selects it unless it is in one already).
    pub(crate) fn show_files_menu(
        &mut self,
        at: &FilesMenuAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let clicked = PathBuf::from(&at.path);
        if let Some(panel) = self.files_panel_entity()
            && !panel.read(cx).is_selected(&clicked)
        {
            panel.update(cx, |panel, cx| panel.select_path(&clicked, cx));
        }
        let (mut paths, root) = self.files_subject(cx);
        if paths.is_empty() {
            paths.push(clicked);
        }
        let is_dir = |path: &Path| {
            self.files_kind(path, cx)
                .map_or(paths.len() == 1 && !at.file, |kind| kind == FsKind::Dir)
        };
        let files: Vec<PathBuf> = paths.iter().filter(|p| !is_dir(p)).cloned().collect();
        let single = paths.len() == 1;
        let first = paths[0].clone();
        let first_is_dir = is_dir(&first);
        let first_open = first_is_dir
            && self.files_panel_entity().is_some_and(|panel| {
                panel
                    .read(cx)
                    .tree()
                    .is_some_and(|tree| tree.is_expanded(&first))
            });

        let item = |label: &'static str, action: FilesRowAction, hint: Option<SharedString>| {
            let (paths, root) = (paths.clone(), root.clone());
            Self::entry(label, move |this, window, cx| {
                this.files_row_action(action, &paths, root.as_deref(), window, cx);
            })
            .hinted(hint)
        };
        let op = |label: &'static str, command: FilesCommand, key: &dyn Action| {
            Self::entry(label, move |this, window, cx| {
                this.files_command(command, window, cx);
            })
            .hinted(files_key_hint(window, key))
        };
        let mut menu: Vec<MenuEntry> = Vec::new();
        let mut group = |entries: Vec<MenuEntry>| {
            let mut entries = entries.into_iter();
            let Some(head) = entries.next() else {
                return;
            };
            menu.push(if menu.is_empty() {
                head
            } else {
                head.starting_group()
            });
            menu.extend(entries);
        };

        let mut open = Vec::new();
        if !files.is_empty() {
            open.push(item("Open", FilesRowAction::Open, Some("↩".into())));
        }
        if single && !first_is_dir {
            open.push(item(
                "Open to the Side",
                FilesRowAction::OpenToSide,
                Some("⌥↩".into()),
            ));
        }
        if self.files_can_reveal(cx) {
            open.push(item("Reveal in Finder", FilesRowAction::Reveal, None));
        }
        group(open);

        let mut compare = Vec::new();
        if single && !first_is_dir {
            compare.push(item(
                "Select for Compare",
                FilesRowAction::SelectForCompare,
                None,
            ));
            if self.compare_source.as_ref().is_some_and(|s| *s != first) {
                compare.push(item(
                    "Compare with Selected",
                    FilesRowAction::CompareWithSelected,
                    None,
                ));
            }
            compare.push(item(
                "Compare with HEAD",
                FilesRowAction::CompareWithHead,
                None,
            ));
        } else if files.len() == 2 && paths.len() == 2 {
            compare.push(item(
                "Compare Selected",
                FilesRowAction::CompareSelected,
                None,
            ));
        }
        group(compare);

        let mut hooks = Vec::new();
        if single {
            hooks.push(item(
                "Open in Terminal",
                FilesRowAction::OpenInTerminal,
                None,
            ));
        }
        if !files.is_empty() {
            hooks.push(item("Attach to Chat", FilesRowAction::AttachToChat, None));
        }
        if single {
            hooks.push(item(
                "Filter in this Folder",
                FilesRowAction::FilterInFolder,
                None,
            ));
            hooks.push(item("Find in Folder", FilesRowAction::FindInFolder, None));
            if !first_is_dir {
                hooks.push(item("Show File History", FilesRowAction::ShowHistory, None));
            }
            if first_open {
                hooks.push(item(
                    "Collapse Folder",
                    FilesRowAction::CollapseFolder,
                    Some("←".into()),
                ));
            }
        }
        group(hooks);

        group(vec![
            item(
                "Copy Path",
                FilesRowAction::CopyPath,
                files_key_hint(window, &actions::FilesCopyPath),
            ),
            item(
                "Copy Relative Path",
                FilesRowAction::CopyRelativePath,
                files_key_hint(window, &actions::FilesCopyRelativePath),
            ),
        ]);

        let mut ops = Vec::new();
        if single {
            ops.push(op(
                "New File",
                FilesCommand::NewFile,
                &actions::FilesNewFile,
            ));
            ops.push(op(
                "New Folder",
                FilesCommand::NewFolder,
                &actions::FilesNewFolder,
            ));
            ops.push(op("Rename", FilesCommand::Rename, &actions::FilesRename));
        }
        ops.push(op(
            "Duplicate",
            FilesCommand::Duplicate,
            &actions::FilesDuplicate,
        ));
        ops.push(op("Cut", FilesCommand::Cut, &actions::FilesCut));
        ops.push(op("Copy", FilesCommand::Copy, &actions::FilesCopy));
        ops.push(op("Paste", FilesCommand::Paste, &actions::FilesPaste));
        ops.push(op(
            "Move to Trash",
            FilesCommand::Trash,
            &actions::FilesTrash,
        ));
        group(ops);

        self.open_menu(at.position, menu, window, cx);
    }

    // ---- the commands ----------------------------------------------------------------------

    /// A `Files: …` command of the palette: the action on the tree's selection.
    pub(crate) fn files_row_command(
        &mut self,
        action: FilesRowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (paths, root) = self.files_subject(cx);
        if paths.is_empty() {
            self.set_notice("Select a file or folder in the Files panel first", cx);
            return;
        }
        self.files_row_action(action, &paths, root.as_deref(), window, cx);
    }

    /// Does `action` with `paths` (what the tree acts on: absolute, a folder standing for what
    /// is inside it), the tree rooted at `root`.
    pub(crate) fn files_row_action(
        &mut self,
        action: FilesRowAction,
        paths: &[PathBuf],
        root: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(first) = paths.first().cloned() else {
            return;
        };
        let first_is_dir = self.files_is_dir(&first, cx);
        // The folder a file's actions are about: itself, or the folder it is in.
        let folder_of = |path: &Path, is_dir: bool| {
            if is_dir {
                Some(path.to_owned())
            } else {
                path.parent().map(Path::to_owned)
            }
        };
        match action {
            FilesRowAction::Open => {
                let files: Vec<PathBuf> = paths
                    .iter()
                    .filter(|p| !self.files_is_dir(p, cx))
                    .cloned()
                    .collect();
                for path in files {
                    self.open_file(FileOpen::picked(path), window, cx);
                }
            }
            FilesRowAction::OpenToSide => {
                if !first_is_dir {
                    self.open_file_placed(FileOpen::picked(first), FilePlace::Split, window, cx);
                }
            }
            FilesRowAction::Reveal => {
                if !self.files_can_reveal(cx) {
                    self.set_notice("Finder cannot show files of another computer", cx);
                    return;
                }
                for path in paths {
                    (self.daemon.reveal.clone())(path, cx);
                }
            }
            FilesRowAction::CopyPath => {
                let text = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.copied_paths(text, paths.len(), cx);
            }
            FilesRowAction::CopyRelativePath => {
                let text = paths
                    .iter()
                    .map(|p| relative_to(p, root))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.copied_paths(text, paths.len(), cx);
            }
            FilesRowAction::OpenInTerminal => {
                let Some(dir) = folder_of(&first, first_is_dir) else {
                    return;
                };
                let workspace = {
                    let store = self.sessions.read(cx);
                    self.target_workspace(store).map(|w| w.id)
                };
                self.start_terminal_in(workspace, Some(dir), window, cx);
            }
            FilesRowAction::AttachToChat => {
                let files: Vec<PathBuf> = paths
                    .iter()
                    .filter(|p| !self.files_is_dir(p, cx))
                    .cloned()
                    .collect();
                if files.is_empty() {
                    self.set_notice("Only files can be attached to a chat", cx);
                    return;
                }
                self.attach_files_to_chat(&files, root, cx);
            }
            FilesRowAction::FilterInFolder => {
                let Some(dir) = folder_of(&first, first_is_dir) else {
                    return;
                };
                if let Some(panel) = self.files_panel_entity() {
                    panel.update(cx, |panel, cx| panel.filter_in_folder(&dir, window, cx));
                }
            }
            FilesRowAction::FindInFolder => {
                let Some(dir) = folder_of(&first, first_is_dir) else {
                    return;
                };
                self.find_in_folder(&dir, root, window, cx);
            }
            FilesRowAction::ShowHistory => {
                if !first_is_dir {
                    self.open_file_history(&first, None, window, cx);
                }
            }
            FilesRowAction::CollapseFolder => {
                if let Some(panel) = self.files_panel_entity() {
                    panel.update(cx, |panel, cx| panel.collapse_folder(&first, cx));
                }
            }
            FilesRowAction::SelectForCompare => {
                if first_is_dir {
                    self.set_notice("Only files can be compared", cx);
                    return;
                }
                let name = hutan_code_view::file_name(&first);
                self.compare_source = Some(first);
                self.set_notice(format!("Selected “{name}” for compare"), cx);
            }
            FilesRowAction::CompareWithSelected => {
                let Some(source) = self.compare_source.clone() else {
                    self.set_notice("Choose Select for Compare on a file first", cx);
                    return;
                };
                if first_is_dir {
                    self.set_notice("Only files can be compared", cx);
                } else if source == first {
                    self.set_notice("That is the file selected for compare: pick another", cx);
                } else {
                    self.open_compare(&source, &first, window, cx);
                }
            }
            FilesRowAction::CompareSelected => match paths {
                [a, b] if !self.files_is_dir(a, cx) && !self.files_is_dir(b, cx) => {
                    self.open_compare(a, b, window, cx);
                }
                _ => self.set_notice("Select two files to compare them", cx),
            },
            FilesRowAction::CompareWithHead => {
                if first_is_dir {
                    self.set_notice("Only files can be compared", cx);
                } else {
                    self.open_compare_head(&first, window, cx);
                }
            }
        }
    }

    /// Puts `text` (`count` paths) on the clipboard and says so.
    fn copied_paths(&mut self, text: String, count: usize, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_notice(
            if count == 1 {
                "Copied the path".to_owned()
            } else {
                format!("Copied {count} paths")
            },
            cx,
        );
    }

    /// The chat Attach to Chat puts files in: the one in the focused pane's active tab, else the
    /// active tab of another pane, else a chat tab behind another tab (a click in the tree
    /// previews a file in the chat's pane, which makes the file the active tab). It need not have
    /// the keyboard: the Files tree has it when its menu runs.
    fn active_chat(&self) -> Option<gpui::Entity<super::chat::ChatView>> {
        let active = self.state.active_pane_id();
        let panes = self.state.center.panes();
        let ordered = panes
            .iter()
            .filter(|p| p.id == active)
            .chain(panes.iter().filter(|p| p.id != active));
        for pane in ordered {
            // The tab on top first, then the ones behind it.
            let tabs = pane.active_item().into_iter().chain(pane.items.iter());
            for item in tabs {
                if let ItemRef::Chat { session } = item
                    && let Ok(session) = session.parse()
                    && let Some(view) = self.chats.get(&(pane.id, session))
                {
                    return Some(view.clone());
                }
            }
        }
        None
    }

    /// *Attach to Chat*: a file chip of each of `files` in the composer of the chat that is open;
    /// with none open, `@path` text of each (relative to the tree's root) is copied for the
    /// next one.
    fn attach_files_to_chat(
        &mut self,
        files: &[PathBuf],
        root: Option<&Path>,
        cx: &mut Context<Self>,
    ) {
        match self.active_chat() {
            Some(chat) => {
                let named: Vec<(PathBuf, String)> = files
                    .iter()
                    .map(|p| (p.clone(), relative_to(p, root)))
                    .collect();
                let added = chat.update(cx, |chat, cx| chat.attach_tree_files(&named, cx));
                if added > 0 {
                    self.set_notice(
                        if added == 1 {
                            "Attached 1 file to the chat".to_owned()
                        } else {
                            format!("Attached {added} files to the chat")
                        },
                        cx,
                    );
                }
            }
            None => {
                let text = files
                    .iter()
                    .map(|p| format!("@{}", relative_to(p, root)))
                    .collect::<Vec<_>>()
                    .join(" ");
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                self.set_notice(format!("No chat is open: copied {text}"), cx);
            }
        }
    }

    /// *Find in Folder*: the Search panel, its include field set to `dir` below the root.
    fn find_in_folder(
        &mut self,
        dir: &Path,
        root: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let relative = relative_to(dir, root);
        let glob = if relative.is_empty() || dir == root.unwrap_or(Path::new("")) {
            String::new()
        } else {
            format!("{}/**", glob_escaped(&relative))
        };
        self.open_panel("search", cx);
        let panel = self
            .panel_view(&super::model::PanelId::new("search"), window, cx)
            .and_then(|view| view.downcast::<SearchPanel>().ok());
        let Some(panel) = panel else {
            self.set_notice("The Search panel is not available", cx);
            return;
        };
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| panel.find_in_folder(&glob, window, cx));
        });
    }
}

/// `path` relative to `root` (the path itself when it is not below it).
fn relative_to(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}
