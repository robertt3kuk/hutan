//! Vim's `:` commands about files, windows, buffers and terminals (slice E-8), run for the file
//! tab they were typed in: `:w` saves through `fs.write` with the conflict rules of E-6, `:q`
//! closes the tab (refusing to drop unsaved changes without `!`), `:wq` / `:x` save and close,
//! `:e` opens a path (relative to the file's folder) or reads the file again, `:sp` / `:vs` split
//! the pane with a second view of the same buffer, `:b` and `:ls` go through the open files by
//! Vim's buffer numbers, and `:term` starts a Hutan terminal in the file's folder, in a split
//! below. The text commands (`:s :g :normal :set :N`) run in the code view itself.

use std::path::{Path, PathBuf};

use gpui::{Context, Entity, Window};
use hutan_code_view::ex::Ex;

use super::file_view::FileView;
use super::files::FileOpen;
use super::model::{Edge, ItemRef, PaneId};
use super::view::Workbench;

/// Vim's "E37": unsaved changes would be lost.
const NOT_SAVED: &str = "E37: No write since last change (add ! to override)";

impl Workbench {
    /// Runs `command`, typed in `view` (a file tab of `pane`).
    pub(crate) fn run_file_command(
        &mut self,
        pane: PaneId,
        view: &Entity<FileView>,
        command: Ex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = view.read(cx).buffer().read(cx).path().to_owned();
        let key = path.display().to_string();
        let item = ItemRef::file(key.clone());
        let dirty = view.read(cx).buffer().read(cx).is_dirty();
        let say = |text: String, error: bool, cx: &mut Context<Self>| {
            view.update(cx, |v, cx| v.say(text, error, cx));
        };
        match command {
            Ex::Write { .. } => self.write_from_vim(&key, None, view, cx),
            Ex::Quit { force } => {
                let index = self
                    .state
                    .pane(pane)
                    .and_then(|p| p.items.iter().position(|i| *i == item));
                let Some(index) = index else {
                    return;
                };
                if !force && self.closing_loses_changes(&key, cx) {
                    say(NOT_SAVED.to_owned(), true, cx);
                    return;
                }
                self.try_mutate(cx, |s| s.close_item(pane, index));
            }
            Ex::WriteQuit { only_if_changed } => {
                if only_if_changed && !dirty {
                    if let Some(index) = self
                        .state
                        .pane(pane)
                        .and_then(|p| p.items.iter().position(|i| *i == item))
                    {
                        self.try_mutate(cx, |s| s.close_item(pane, index));
                    }
                } else {
                    self.write_from_vim(&key, Some(item), view, cx);
                }
            }
            Ex::Edit { path: None, force } => {
                if dirty && !force {
                    say(NOT_SAVED.to_owned(), true, cx);
                } else {
                    // `:e!` drops the changes: the file as it is on disk (⌘Z brings them back).
                    self.reload_file(&key, cx);
                }
            }
            Ex::Edit {
                path: Some(target), ..
            } => {
                let target = resolve(&path, &target);
                self.open_file(FileOpen::picked(target), window, cx);
            }
            Ex::Split { vertical } => {
                let edge = if vertical { Edge::Right } else { Edge::Bottom };
                self.open_file_in_split(pane, &key, edge, window, cx);
            }
            Ex::Buffer(which) => match self.buffer_named(&which) {
                Ok(target) => self.open_file(FileOpen::picked(target), window, cx),
                Err(error) => say(error, true, cx),
            },
            Ex::Buffers => {
                let list = self.buffer_list(&key, cx);
                say(list, false, cx);
            }
            Ex::Terminal => {
                let dir = path.parent().map(Path::to_path_buf);
                self.split_pane_with_terminal_in(pane, Edge::Bottom, dir, window, cx);
            }
            // The text commands run in the code view; `:noh` in the file tab.
            Ex::NoHighlight
            | Ex::Set(_)
            | Ex::Goto(_)
            | Ex::Substitute { .. }
            | Ex::Global { .. }
            | Ex::Normal { .. } => {}
        }
    }

    /// `:w`: the file is saved as ⌘S saves it (the precondition of E-6: a file changed on disk
    /// is never overwritten; the conflict bar asks). `then_close`: `:wq`.
    fn write_from_vim(
        &mut self,
        key: &str,
        then_close: Option<ItemRef>,
        view: &Entity<FileView>,
        cx: &mut Context<Self>,
    ) {
        let buffer = view.read(cx).buffer().clone();
        let (editable, conflict) = {
            let b = buffer.read(cx);
            (b.is_editable(), b.conflict().is_some())
        };
        let answer = if !editable {
            Some(("E45: The file is read-only".to_owned(), true))
        } else if conflict {
            Some((
                "The file changed on disk: choose Reload or Keep Mine in the bar above it"
                    .to_owned(),
                true,
            ))
        } else {
            None
        };
        if let Some((text, error)) = answer {
            view.update(cx, |v, cx| v.say(text, error, cx));
            return;
        }
        self.save_file(key, then_close, cx);
    }

    /// The open file `:b <which>` names: a buffer number, or a part of a file's path that only
    /// one open file has.
    fn buffer_named(&self, which: &str) -> Result<PathBuf, String> {
        if let Ok(number) = which.parse::<usize>() {
            return self
                .buffers
                .buffers
                .iter()
                .find(|(_, s)| s.number == number)
                .map(|(path, _)| PathBuf::from(path))
                .ok_or_else(|| format!("E86: Buffer {number} does not exist"));
        }
        let matches: Vec<&String> = self
            .buffers
            .buffers
            .keys()
            .filter(|path| path.contains(which))
            .collect();
        match matches.as_slice() {
            [one] => Ok(PathBuf::from(one.as_str())),
            [] => Err(format!("E94: No matching buffer for {which}")),
            _ => Err(format!("E93: More than one match for {which}")),
        }
    }

    /// `:ls`: every open file with its number, `%` for this one and `+` for unsaved changes.
    fn buffer_list(&self, current: &str, cx: &gpui::App) -> String {
        let mut entries: Vec<(usize, String)> = self
            .buffers
            .buffers
            .iter()
            .map(|(path, stored)| {
                let mark = if path == current { "%" } else { "" };
                let dirty = if stored.buffer.read(cx).is_dirty() {
                    " +"
                } else {
                    ""
                };
                let name = hutan_code_view::file_name(Path::new(path));
                (
                    stored.number,
                    format!("{}{mark} {name}{dirty}", stored.number),
                )
            })
            .collect();
        entries.sort();
        entries
            .into_iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>()
            .join("  ·  ")
    }
}

/// `target` as typed after `:e`: `~` is the home folder, a relative path is relative to the
/// folder of the file the command was typed in.
fn resolve(file: &Path, target: &str) -> PathBuf {
    if let Some(rest) = target.strip_prefix("~/")
        && let Some(home) = std::env::home_dir()
    {
        return home.join(rest);
    }
    let target = Path::new(target);
    if target.is_absolute() {
        return target.to_path_buf();
    }
    file.parent()
        .map_or_else(|| target.to_path_buf(), |dir| dir.join(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_relative_to_the_file() {
        let file = Path::new("/w/src/lib.rs");
        assert_eq!(resolve(file, "main.rs"), PathBuf::from("/w/src/main.rs"));
        assert_eq!(
            resolve(file, "../README.md"),
            PathBuf::from("/w/src/../README.md")
        );
        assert_eq!(resolve(file, "/etc/hosts"), PathBuf::from("/etc/hosts"));
    }
}
