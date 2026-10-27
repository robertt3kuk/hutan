//! The Changes view's file list (FX-CHANGES-VIEW): a row per file with its **name first** and its
//! folder muted after it (the folder shortened in the middle when there is not room, the name
//! never), the full path in the row's tooltip; or, as a tree, the files grouped under folders
//! that fold and count what is inside them. The list scrolls on its own, has a scrollbar, and
//! moves with the arrow keys.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use hutan_protocol::worktree::{DiffFile, DiffStatus};

use super::{ChangesView, FilePatch, IndexFilter, memory_files_hint, status_mark};
use crate::settings::{ChangesList, ChangesSettings};
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::{Look, counts_of};
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;

/// Indent of one folder level in the tree, in pixels.
const INDENT: f32 = 14.0;

/// One row of the list as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    /// A folder of the tree (a chain of folders with one child each is one row).
    Dir {
        /// The folder's path, which folds it.
        path: String,
        /// What the row says: `notes` or `notes/deep`.
        name: String,
        depth: usize,
        files: usize,
        added: u64,
        removed: u64,
        folded: bool,
    },
    /// File `index` of the listing.
    File { index: usize, depth: usize },
}

/// The folder and the name of a path: `a/b/c.md` → (`a/b`, `c.md`), `c.md` → (``, `c.md`).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => ("", path),
    }
}

/// What a flat row says first and after it: the file's **name** (`old → new` names for a rename)
/// and its folder, muted. The name is never shortened; the folder is, when the row is short of
/// room.
pub fn row_parts(path: &str, old_path: Option<&str>) -> (String, String) {
    let (folder, name) = split_path(path);
    let name = match old_path {
        Some(old) => format!("{} → {name}", split_path(old).1),
        None => name.to_owned(),
    };
    (name, folder.to_owned())
}

/// `folder` shortened to at most `max` characters by dropping the middle: whole folders first
/// (`a/…/z`), then characters (`abc…xyz`). A folder that fits is returned as it is.
pub fn middle_ellipsis(folder: &str, max: usize) -> String {
    let count = folder.chars().count();
    if count <= max {
        return folder.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let parts: Vec<&str> = folder.split('/').collect();
    if parts.len() >= 3
        && let (Some(first), Some(last)) = (parts.first(), parts.last())
    {
        let squeezed = format!("{first}/…/{last}");
        if squeezed.chars().count() <= max {
            // Keep as many of the last folders as still fit.
            let mut tail: Vec<&str> = vec![last];
            for part in parts[1..parts.len() - 1].iter().rev() {
                let candidate = format!("{first}/…/{part}/{}", tail.join("/"));
                if candidate.chars().count() > max {
                    break;
                }
                tail.insert(0, part);
            }
            return format!("{first}/…/{}", tail.join("/"));
        }
    }
    if max == 1 {
        return "…".to_owned();
    }
    let keep = max - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let chars: Vec<char> = folder.chars().collect();
    let mut shortened: String = chars[..head].iter().collect();
    shortened.push('…');
    shortened.extend(&chars[chars.len() - tail..]);
    shortened
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
}

impl Node {
    fn insert(&mut self, folders: &[&str], index: usize) {
        match folders.split_first() {
            None => self.files.push(index),
            Some((first, rest)) => self
                .dirs
                .entry((*first).to_owned())
                .or_default()
                .insert(rest, index),
        }
    }

    /// `(files, added, removed)` of everything under this node.
    fn totals(&self, files: &[DiffFile]) -> (usize, u64, u64) {
        let mut sum = (self.files.len(), 0u64, 0u64);
        for &index in &self.files {
            sum.1 += u64::from(files[index].additions);
            sum.2 += u64::from(files[index].deletions);
        }
        for dir in self.dirs.values() {
            let (n, a, r) = dir.totals(files);
            sum.0 += n;
            sum.1 += a;
            sum.2 += r;
        }
        sum
    }
}

/// The rows of a tree over `files` (`shown` are the indexes to list): folders first, then files,
/// each in name order; a folder in `folded` hides what is under it.
pub fn tree_rows(
    files: &[DiffFile],
    shown: &[usize],
    folded: &std::collections::HashSet<String>,
) -> Vec<ListRow> {
    let mut root = Node::default();
    for &index in shown {
        let (folder, _) = split_path(&files[index].path);
        let parts: Vec<&str> = if folder.is_empty() {
            Vec::new()
        } else {
            folder.split('/').collect()
        };
        root.insert(&parts, index);
    }
    let mut rows = Vec::new();
    emit(&root, files, folded, "", 0, &mut rows);
    rows
}

fn emit(
    node: &Node,
    files: &[DiffFile],
    folded: &std::collections::HashSet<String>,
    prefix: &str,
    depth: usize,
    rows: &mut Vec<ListRow>,
) {
    for (name, dir) in &node.dirs {
        // A folder whose only child is a folder joins it: `notes/deep`.
        let mut label = name.clone();
        let mut path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let mut leaf = dir;
        while leaf.files.is_empty() && leaf.dirs.len() == 1 {
            let Some((next, inner)) = leaf.dirs.iter().next() else {
                break;
            };
            label = format!("{label}/{next}");
            path = format!("{path}/{next}");
            leaf = inner;
        }
        let (count, added, removed) = leaf.totals(files);
        let is_folded = folded.contains(&path);
        rows.push(ListRow::Dir {
            path: path.clone(),
            name: label,
            depth,
            files: count,
            added,
            removed,
            folded: is_folded,
        });
        if !is_folded {
            emit(leaf, files, folded, &path, depth + 1, rows);
        }
    }
    let mut own = node.files.clone();
    own.sort_by(|a, b| {
        split_path(&files[*a].path)
            .1
            .cmp(split_path(&files[*b].path).1)
    });
    for index in own {
        rows.push(ListRow::File { index, depth });
    }
}

impl ChangesView {
    /// How the list is laid out now: the setting, unless the list's own toggle said otherwise.
    pub fn list_mode(&self) -> ChangesList {
        self.list_override
            .unwrap_or_else(|| self.env.settings.get::<ChangesSettings>().list)
    }

    /// Switches the list between flat and tree for this view (the setting is not written).
    pub fn set_list_mode(&mut self, mode: ChangesList, cx: &mut Context<Self>) {
        self.list_override = Some(mode);
        cx.notify();
    }

    /// Folds or unfolds the folder `path` of the tree.
    pub fn toggle_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.folded_dirs.remove(path) {
            self.folded_dirs.insert(path.to_owned());
        }
        cx.notify();
    }

    /// The folders of the tree that are folded.
    pub fn folded_folders(&self) -> Vec<String> {
        let mut folded: Vec<String> = self.folded_dirs.iter().cloned().collect();
        folded.sort();
        folded
    }

    fn shown_indexes(&self) -> Vec<usize> {
        let filter = if self.has_index_marks() {
            self.index_filter
        } else {
            IndexFilter::All
        };
        self.files()
            .iter()
            .enumerate()
            .filter(|(_, f)| filter.shows(f))
            .map(|(i, _)| i)
            .collect()
    }

    /// The rows of the list as drawn, top to bottom.
    pub fn list_rows(&self) -> Vec<ListRow> {
        let shown = self.shown_indexes();
        match self.list_mode() {
            ChangesList::Flat => shown
                .into_iter()
                .map(|index| ListRow::File { index, depth: 0 })
                .collect(),
            ChangesList::Tree => tree_rows(self.files(), &shown, &self.folded_dirs),
        }
    }

    /// Moves the selection to the file above or below the selected one in the list as drawn,
    /// and scrolls the list to it.
    pub fn select_neighbor(&mut self, forward: bool, cx: &mut Context<Self>) {
        let rows = self.list_rows();
        let files: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(at, row)| match row {
                ListRow::File { index, .. } => Some((at, *index)),
                ListRow::Dir { .. } => None,
            })
            .collect();
        let current = self.selected.as_deref().and_then(|selected| {
            files
                .iter()
                .position(|(_, index)| self.files()[*index].path == selected)
        });
        let next = match (current, forward) {
            (Some(at), true) => (at + 1).min(files.len().saturating_sub(1)),
            (Some(at), false) => at.saturating_sub(1),
            (None, _) => 0,
        };
        let Some(&(row, index)) = files.get(next) else {
            return;
        };
        let path = self.files()[index].path.clone();
        self.select(&path, cx);
        self.files_scroll.scroll_to_item(row);
        cx.notify();
    }

    pub(super) fn render_files(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::of(cx);
        let mode = self.list_mode();
        let rows = self.list_rows();
        let width = if wide {
            Some(self.list_width(tokens))
        } else {
            None
        };
        // How many characters of a folder the row has room for after the name (a guess from the
        // width of the list: a mono-ish 0.55 em per character, 210 px of marks and counts).
        let room = width.unwrap_or_else(|| self.width.get().map_or(600.0, f32::from));
        let char_px = tokens.font_size * 0.55;
        let mut list = div()
            .id("changes-files")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.files_scroll)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.keys_to_list(window, cx);
                }),
            )
            .py(px(tokens.spacing));
        for row in &rows {
            list = list.child(match row {
                ListRow::Dir {
                    path,
                    name,
                    depth,
                    files,
                    added,
                    removed,
                    folded,
                } => self.render_dir_row(
                    path, name, *depth, *files, *added, *removed, *folded, &look, theme, tokens, cx,
                ),
                ListRow::File { index, depth } => {
                    let file = &self.files()[*index];
                    let name_chars = split_path(&file.path).1.chars().count();
                    let budget = ((room - 210.0) / char_px) as isize - name_chars as isize;
                    self.render_file_row(
                        *index,
                        file,
                        *depth,
                        mode == ChangesList::Tree,
                        budget.max(0) as usize,
                        &look,
                        theme,
                        tokens,
                        cx,
                    )
                }
            });
        }
        let toolbar = self.render_list_toolbar(mode, rows.len(), theme, tokens, cx);
        let mut column = div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .child(toolbar)
            .child(
                // The scrolled viewport, which a probe inside the scrolled list could not give:
                // the rest of the column beside the diff; under the strip, as tall as its rows
                // up to eight.
                div()
                    .relative()
                    .min_h_0()
                    .when(wide, |s| s.flex_1())
                    .when(!wide, |s| {
                        s.h(px(
                            tokens.sidebar_row * rows.len().min(8) as f32 + tokens.spacing * 2.0
                        ))
                    })
                    .child(list)
                    .child(probe(&self.env.regions, Region::Changes("list"))),
            )
            .child(self.files_bar.element(theme));
        column = match width {
            Some(width) => column.w(px(width)).h_full(),
            // Unfolded under the strip: the whole width, at most eight rows tall.
            None => column.w_full(),
        };
        column.into_any_element()
    }

    /// The width of the list column beside the diff.
    pub(super) fn list_width(&self, tokens: &Tokens) -> f32 {
        let view = self.width.get().map_or(1000.0, f32::from);
        // The diff keeps at least 260 px; the list never takes more than that leaves.
        self.split.width(
            (tokens.font_size * 34.0).min(view * 0.4),
            view - 260.0 - ui::SPLITTER_HANDLE,
        )
    }

    fn render_list_toolbar(
        &self,
        mode: ChangesList,
        rows: usize,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tree = mode == ChangesList::Tree;
        let toggle = ui::IconButton::new(
            "changes-list-mode",
            if tree {
                crate::icons::Icon::ItemFile
            } else {
                crate::icons::Icon::ActionSubagents
            },
            if tree {
                "List files flat"
            } else {
                "Group files by folder"
            },
            theme,
            tokens,
        )
        .compact()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_list_mode(
                if tree {
                    ChangesList::Flat
                } else {
                    ChangesList::Tree
                },
                cx,
            );
        }))
        .child(probe(&self.env.regions, Region::Changes("list-mode")));
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(if tree {
                        "By folder".to_owned()
                    } else {
                        format!("{rows} files")
                    })),
            )
            .child(toggle)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // what a folder row says and how it is drawn
    fn render_dir_row(
        &self,
        path: &str,
        name: &str,
        depth: usize,
        files: usize,
        added: u64,
        removed: u64,
        folded: bool,
        look: &Look,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let target = path.to_owned();
        let tip = SharedString::from(format!("{path}/"));
        div()
            .id(SharedString::from(format!("changes-dir-{path}")))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .mx(px(tokens.spacing))
            .pl(px(tokens.spacing * 1.5 + INDENT * depth as f32))
            .pr(px(tokens.spacing * 1.5))
            .h(px(tokens.sidebar_row))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_folder(&target, cx)))
            .child(div().flex_none().w(px(12.0)).text_color(theme.muted).child(
                crate::icons::glyph(if folded {
                    crate::icons::Icon::ActionDisclose
                } else {
                    crate::icons::Icon::ActionExpand
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .medium()
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(files.to_string())),
            )
            .child(counts_of(added as usize, removed as usize, look))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // what a file row says and how it is drawn
    fn render_file_row(
        &self,
        index: usize,
        file: &DiffFile,
        depth: usize,
        in_tree: bool,
        folder_room: usize,
        look: &Look,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let home = self.env.sessions.read(cx).home_folder();
        let hint = memory_files_hint(
            home.as_deref(),
            &[self.path.as_path(), self.canonical.as_path()],
            &file.path,
        );
        let (mark, color) = status_mark(file.status, theme);
        // staged / unstaged / both: a 14 px glyph with the word in its tooltip (a state is not a
        // pill that doubles the row's marks): a check for staged, a dot in the warning colour
        // while something is left unstaged.
        let index_glyph = super::index_mark(file).map(|word| {
            let (icon, tone, tip) = match word {
                "staged" => (crate::icons::Icon::StateCompleted, theme.success, "Staged"),
                "unstaged" => (crate::icons::Icon::VcsDirty, theme.warning, "Unstaged"),
                _ => (
                    crate::icons::Icon::VcsDirty,
                    theme.warning,
                    "Staged and unstaged changes",
                ),
            };
            div()
                .id(SharedString::from(format!("changes-index-glyph-{index}")))
                .flex_none()
                .w(px(14.0))
                .text_color(tone)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(SharedString::from(tip))).into())
                .child(crate::icons::glyph(icon))
        });
        let selected = self.selected.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        // The whole path (and where a renamed file came from) is the row's tooltip.
        let tip = SharedString::from(match &file.old_path {
            Some(old) => format!("{}\nrenamed from {old}", file.path),
            None => file.path.clone(),
        });
        let (name_text, folder) = row_parts(&file.path, file.old_path.as_deref());
        // "Open": the file in a tab, at its first hunk once its patch is read (E-10).
        let first_line = match self.patches.get(&file.path) {
            Some(FilePatch::Ready { diff, .. }) => diff.first_changed_line(),
            _ => None,
        };
        // Shown on hover (and on the selected row), so a list of files is not a list of buttons.
        let row_group = SharedString::from(format!("changes-row-{index}"));
        let open = (file.status != DiffStatus::Deleted).then(|| {
            let button = self.open_button(
                SharedString::from(format!("changes-open-{index}")),
                &file.path,
                first_line,
                Region::ChangesOpen(index),
                theme,
                tokens,
            );
            div()
                .flex()
                .flex_none()
                .items_center()
                .when(!selected, |s| {
                    s.opacity(0.0)
                        .group_hover(row_group.clone(), |s| s.opacity(1.0))
                })
                .child(button)
                .child(self.show_in_files_button(
                    SharedString::from(format!("changes-files-{index}")),
                    &file.path,
                    Region::ChangesShowInFiles(index),
                    theme,
                    tokens,
                ))
        });
        let folder_text =
            (!in_tree && !folder.is_empty()).then(|| middle_ellipsis(&folder, folder_room));
        div()
            .id(SharedString::from(format!("changes-file-{index}")))
            .group(SharedString::from(format!("changes-row-{index}")))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .mx(px(tokens.spacing))
            .pl(px(tokens.spacing * 1.5 + INDENT * depth as f32))
            .pr(px(tokens.spacing * 1.5))
            .h(px(tokens.sidebar_row))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select(&path, cx);
                this.keys_to_list(window, cx);
            }))
            .children(self.render_file_check(index, file, theme, cx))
            .child(div().flex_none().w(px(12.0)).text_color(color).child(mark))
            .child(
                // The name is never cut; the folder after it gives way (`truncate` is the
                // backstop for the folder when the estimate of its room was generous).
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(tokens.spacing * 1.5))
                    .child(div().flex_none().child(SharedString::from(name_text)))
                    .children(folder_text.map(|folder| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(folder))
                    }))
                    .children(hint.map(|hint| {
                        div()
                            .id(SharedString::from(format!("changes-memory-hint-{index}")))
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(hint)
                            .child(probe(&self.env.regions, Region::ChangesMemoryHint(index)))
                    })),
            )
            .children(index_glyph)
            .child(if file.binary {
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child("binary")
                    .into_any_element()
            } else {
                counts_of(file.additions as usize, file.deletions as usize, look)
            })
            .children(open)
            .child(probe(&self.env.regions, Region::ChangesFile(index)))
            .into_any_element()
    }

    /// *Show in Files*: the file selected in the Files panel, which works in this view's
    /// checkout from then on.
    pub(super) fn show_in_files_button(
        &self,
        id: SharedString,
        file: &str,
        region: Region,
        theme: &Theme,
        tokens: &Tokens,
    ) -> AnyElement {
        let target = crate::workbench::actions::ChangedFileAt {
            checkout: self.path.display().to_string(),
            file: file.to_owned(),
        };
        ui::IconButton::new(
            id,
            crate::icons::Icon::ItemFolderOpen,
            "Show in Files",
            theme,
            tokens,
        )
        .compact()
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(
                Box::new(crate::workbench::actions::ShowInFiles(target.clone())),
                cx,
            );
        })
        .child(probe(&self.env.regions, region))
        .into_any_element()
    }

    /// The keys act on the file list (arrows move the selection, page keys scroll the list):
    /// after a click in it.
    pub(super) fn keys_to_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keys_on_list = true;
        let handle = self.review.focus.clone();
        cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn file(path: &str) -> DiffFile {
        DiffFile {
            path: path.to_owned(),
            old_path: None,
            status: DiffStatus::Added,
            additions: 3,
            deletions: 1,
            binary: false,
            committed: false,
            uncommitted: true,
            staged: false,
            unstaged: false,
        }
    }

    #[test]
    fn a_path_splits_into_its_folder_and_its_name() {
        assert_eq!(split_path("a/b/c.md"), ("a/b", "c.md"));
        assert_eq!(split_path("c.md"), ("", "c.md"));
    }

    #[test]
    fn a_row_says_the_name_first_and_the_folder_after_it() {
        assert_eq!(
            row_parts("PRJ-3814/notes/plan-04.md", None),
            ("plan-04.md".to_owned(), "PRJ-3814/notes".to_owned())
        );
        assert_eq!(
            row_parts("docs/new.md", Some("old/dir/old.md")),
            ("old.md → new.md".to_owned(), "docs".to_owned())
        );
        assert_eq!(
            row_parts("top.md", None),
            ("top.md".to_owned(), String::new())
        );
    }

    #[test]
    fn a_folder_that_fits_stays_and_one_that_does_not_loses_its_middle() {
        assert_eq!(middle_ellipsis("a/b", 10), "a/b");
        assert_eq!(
            middle_ellipsis("PRJ-3814/notes/deep/er", 14),
            "PRJ-3814/…/er"
        );
        assert_eq!(middle_ellipsis("PRJ-3814/notes/deep", 12), "PRJ-38…/deep");
        let squeezed = middle_ellipsis("a-very-long-folder-name", 9);
        assert_eq!(squeezed.chars().count(), 9);
        assert!(squeezed.contains('…'));
        assert_eq!(middle_ellipsis("anything", 0), "");
        assert_eq!(middle_ellipsis("anything", 1), "…");
    }

    #[test]
    fn the_tree_groups_by_folder_folds_and_counts() {
        let files = vec![
            file("a/x.md"),
            file("a/notes/deep/y.md"),
            file("a/notes/deep/z.md"),
            file("top.md"),
        ];
        let shown: Vec<usize> = (0..files.len()).collect();
        let rows = tree_rows(&files, &shown, &HashSet::new());
        // `a` has files and a folder; `notes/deep` is one row.
        assert_eq!(
            rows.iter()
                .map(|r| match r {
                    ListRow::Dir {
                        name, depth, files, ..
                    } => format!("dir {name} @{depth} ({files})"),
                    ListRow::File { index, depth } => format!("file {index} @{depth}"),
                })
                .collect::<Vec<_>>(),
            vec![
                "dir a @0 (3)",
                "dir notes/deep @1 (2)",
                "file 1 @2",
                "file 2 @2",
                "file 0 @1",
                "file 3 @0",
            ]
        );
        let folded: HashSet<String> = ["a/notes/deep".to_owned()].into();
        let rows = tree_rows(&files, &shown, &folded);
        assert_eq!(rows.len(), 4, "the folded folder hides its two files");
        assert!(matches!(
            &rows[1],
            ListRow::Dir {
                folded: true,
                files: 2,
                added: 6,
                removed: 2,
                ..
            }
        ));
    }
}
