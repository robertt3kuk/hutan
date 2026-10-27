//! The workbench's half of the Files tree's file operations (EX-7, `panels/files/ops.rs`).
//!
//! The panel asks the daemon and keeps the tree; what touches the rest of the window is here:
//!
//! - **The sheets.** *Move to Trash* always asks, and says what the user could lose: the names,
//!   the tabs with unsaved changes (their text stays in the tab), an agent working in the
//!   checkout. The first drag of a session asks too (`panels.files.confirm_drag`).
//! - **Tabs follow a rename or a move.** The buffer of every open file at (or below) the old path
//!   is kept, text and unsaved changes with it, and belongs to the new path: the tab, its views, the
//!   language server's document (closed under the old path, opened under the new one with the
//!   buffer's text), the watch, the recent files. A read of the file at its new place follows, so
//!   an agent's write that raced the move is seen (a clean buffer reloads, a changed one gets the
//!   bar).
//! - **Tabs of what went to the Trash** say the file is deleted (the bar every deleted file
//!   has); what comes back from the Trash takes the bar away.
//!
//! The daemon's `fs.changed` for these operations carries origin `self`, which tabs ignore as
//! they do for their own saves; this is why the panel tells the workbench itself.

use std::path::{Path, PathBuf};

use gpui::{Context, Window};
use hutan_code_view::Conflict;

use super::actions::FilesMove;
use super::lsp::spoken_list;
use super::model::retargeted;
use super::panels::{FilesCommand, FilesPanel, files_listed_names};
use super::view::Workbench;

impl Workbench {
    /// Runs one of the `Files: …` commands on the Files panel (opened when it is closed).
    pub(crate) fn files_command(
        &mut self,
        command: FilesCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.files_panel(window, cx) else {
            self.set_notice("The Files panel is not available", cx);
            return;
        };
        panel.update(cx, |panel, cx| panel.run_command(command, window, cx));
    }

    /// The tabs of `paths` (and of what is inside folders among them) that hold unsaved changes,
    /// by file name.
    fn dirty_tabs_under(&self, paths: &[PathBuf], cx: &gpui::App) -> Vec<String> {
        let mut names: Vec<String> = self
            .buffers
            .buffers
            .iter()
            .filter(|(key, stored)| {
                paths.iter().any(|p| Path::new(key.as_str()).starts_with(p))
                    && stored.buffer.read(cx).is_dirty()
            })
            .map(|(key, _)| hutan_code_view::file_name(Path::new(key)))
            .collect();
        names.sort();
        names
    }

    /// The sheet of *Move to Trash*.
    pub(crate) fn files_ask_trash(
        &mut self,
        paths: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let Some(panel) = self.panel::<FilesPanel>("files") else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let (names, more) = files_listed_names(&paths);
        let title = match paths.as_slice() {
            [only] => format!(
                "Move “{}” to the Trash?",
                only.file_name().map_or_else(
                    || only.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            ),
            many => format!("Move {} items to the Trash?", many.len()),
        };
        let mut message = String::new();
        if paths.len() > 1 {
            for name in &names {
                message.push_str(&format!("• {name}\n"));
            }
            if more > 0 {
                message.push_str(&format!("and {more} more\n"));
            }
            message.push('\n');
        }
        let dirty = self.dirty_tabs_under(&paths, cx);
        if !dirty.is_empty() {
            let tabs = if dirty.len() == 1 {
                "1 tab"
            } else {
                &format!("{} tabs", dirty.len())
            };
            message.push_str(&format!(
                "Unsaved changes in {tabs} ({}): the text stays in the tab, which then says its file is deleted.\n",
                spoken_list(&dirty)
            ));
        }
        let agents = self.sessions.read(cx).agents_working_near(&paths);
        if !agents.is_empty() {
            let mut who: Vec<String> = agents
                .into_iter()
                .map(|(title, agent)| match agent {
                    Some(agent) if super::harnesses::HARNESSES.iter().any(|h| h.id == agent) => {
                        super::harnesses::harness_name(&agent)
                    }
                    _ => title,
                })
                .collect();
            who.sort();
            who.dedup();
            let verb = if who.len() == 1 { "is" } else { "are" };
            message.push_str(&format!(
                "{} {verb} working in this checkout.\n",
                spoken_list(&who)
            ));
        }
        message.push_str(
            "The Trash keeps it: ⌘Z in the Files panel (or Finder's Put Back) brings it back.",
        );
        self.open_destructive(
            title,
            message,
            "Move to Trash",
            move |_, window, cx| {
                let paths = paths.clone();
                panel.update(cx, |panel, cx| panel.trash_confirmed(paths, window, cx));
            },
            window,
            cx,
        );
    }

    /// The sheet of the first drag of a session.
    pub(crate) fn files_ask_move(
        &mut self,
        ask: &FilesMove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<FilesPanel>("files") else {
            return;
        };
        let name_of = |p: &str| {
            Path::new(p)
                .file_name()
                .map_or_else(|| p.to_owned(), |n| n.to_string_lossy().into_owned())
        };
        let what = match ask.items.as_slice() {
            [only] => format!("“{}”", name_of(only)),
            many => format!("{} items", many.len()),
        };
        let verb = if ask.copy { "Copy" } else { "Move" };
        let title = format!("{verb} {what} into “{}”?", name_of(&ask.dir));
        let message = "It happens on disk, not only in the tree. ⌘Z in the Files panel takes it back. You are asked once per session (panels.files.confirm_drag).";
        let ask = ask.clone();
        self.open_confirm(
            title,
            message,
            verb,
            move |_, window, cx| {
                let ask = ask.clone();
                panel.update(cx, |panel, cx| panel.move_confirmed(&ask, window, cx));
            },
            window,
            cx,
        );
    }

    /// Files or folders were renamed or moved: the tabs follow (see the module documentation).
    pub(crate) fn files_tabs_moved(
        &mut self,
        pairs: &[(String, String)],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (from, to) in pairs {
            let keys: Vec<(String, String)> = self
                .buffers
                .buffers
                .keys()
                .filter_map(|key| retargeted(key, from, to).map(|new| (key.clone(), new)))
                .collect();
            for (old, new) in &keys {
                self.rekey_buffer(old, new, cx);
            }
            let views: Vec<_> = self
                .file_views
                .keys()
                .filter_map(|(pane, path)| {
                    retargeted(path, from, to).map(|new| ((*pane, path.clone()), (*pane, new)))
                })
                .collect();
            for (old, new) in views {
                if let Some(view) = self.file_views.remove(&old) {
                    self.file_views.insert(new.clone(), view);
                }
                if let Some(subscription) = self.file_subscriptions.remove(&old) {
                    self.file_subscriptions.insert(new, subscription);
                }
            }
            for recent in &mut self.recent_files {
                if let Some(new) = retargeted(recent, from, to) {
                    *recent = new;
                }
            }
            // The buffers and views are under their new keys: now the layout may point at them.
            self.mutate(cx, |state| {
                state.retarget_files(from, to);
            });
            // A write that raced the move (an agent's) is found now: a clean buffer reloads, a
            // changed one gets the bar.
            for (_, new) in &keys {
                self.read_into(new, false, None, cx);
            }
        }
    }

    /// The buffers of `paths` (or below them) that are ready, with their keys.
    fn buffers_under(&self, paths: &[String]) -> Vec<String> {
        self.buffers
            .buffers
            .keys()
            .filter(|key| {
                paths
                    .iter()
                    .any(|p| Path::new(key.as_str()).starts_with(Path::new(p)))
            })
            .cloned()
            .collect()
    }

    /// These paths went to the Trash: their tabs say the file is deleted and keep their text.
    pub(crate) fn files_tabs_trashed(&mut self, paths: &[String], cx: &mut Context<Self>) {
        for key in self.buffers_under(paths) {
            let Some(stored) = self.buffers.buffers.get_mut(&key) else {
                continue;
            };
            let buffer = stored.buffer.clone();
            if !buffer.read(cx).is_ready() {
                continue;
            }
            stored.conflict_disk = None;
            buffer.update(cx, |b, cx| b.set_conflict(Some(Conflict::Deleted), cx));
        }
    }

    /// These paths came back from the Trash: their tabs find their file again (and read it, in
    /// case it is not what the tab had).
    pub(crate) fn files_tabs_restored(&mut self, paths: &[String], cx: &mut Context<Self>) {
        for key in self.buffers_under(paths) {
            let Some(stored) = self.buffers.buffers.get(&key) else {
                continue;
            };
            let buffer = stored.buffer.clone();
            let deleted = matches!(buffer.read(cx).conflict(), Some(Conflict::Deleted));
            if !deleted {
                continue;
            }
            buffer.update(cx, |b, cx| b.set_conflict(None, cx));
            self.read_into(&key, false, None, cx);
        }
    }
}
