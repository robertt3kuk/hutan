//! Dragging rows of the Files tree to other folders (EX-7): a drop moves them (⌥ copies), a folder
//! under the pointer opens after a moment (spring-loaded), and the tree scrolls by itself while
//! the pointer is near its top or bottom edge.
//!
//! The first drop of a session asks (`panels.files.confirm_drag`): the workbench shows the sheet
//! and answers with [`FilesPanel::move_confirmed`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Bounds, Context, Pixels, Point, SharedString, Window};

use super::super::files_names::{drop_folder, edge_scroll, pointless_move};
use super::super::files_tree::RowKind;
use super::FilesPanel;
use crate::settings::FilesPanelSettings;
use crate::workbench::actions::{FilesAskMove, FilesMove};

/// How long a dragged row rests on a closed folder before the folder opens.
pub const SPRING_AFTER: Duration = Duration::from_millis(700);
/// How often the tree scrolls while a drag is at its edge.
pub const AUTOSCROLL_EVERY: Duration = Duration::from_millis(30);

/// The rows being dragged.
#[derive(Clone, Debug)]
pub struct DraggedRows {
    pub paths: Vec<PathBuf>,
    /// What the floating chip says: the name, or how many.
    pub label: SharedString,
}

impl FilesPanel {
    /// The drag payload of the row at `index`: the row as the user sees it (a folded chain is its
    /// last folder).
    pub(super) fn dragged_by(&self, index: usize) -> Option<DraggedRows> {
        let row = self.rows().get(index)?;
        if matches!(row.kind, RowKind::More | RowKind::Failed) {
            return None;
        }
        let path = row.folder().to_owned();
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Some(DraggedRows {
            paths: vec![path],
            label: name.into(),
        })
    }

    /// Whether `path` is the folder a drag is hovering (it shows the drop).
    pub fn drop_target(&self) -> Option<&Path> {
        self.spring.as_ref().map(|(path, _, _)| path.as_path())
    }

    /// The pointer is over the folder `path` with rows dragged: after [`SPRING_AFTER`] a closed
    /// one opens. Over anything else the wait starts over.
    pub(super) fn drag_over_row(
        &mut self,
        path: Option<(PathBuf, Bounds<Pixels>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = match (&self.spring, &path) {
            (Some((current, _, _)), Some((new, _))) => current == new,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.spring = None;
        let Some((folder, bounds)) = path else {
            return;
        };
        if self.tree.as_ref().is_none_or(|t| t.is_expanded(&folder)) {
            // Open already: nothing to wait for (the highlight is the row's own).
            self.spring = Some((folder, bounds, None));
            return;
        }
        let wait = folder.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SPRING_AFTER).await;
            this.update_in(cx, |this, window, cx| {
                let still = this.spring.as_ref().is_some_and(|(p, bounds, _)| {
                    *p == wait && bounds.contains(&window.mouse_position())
                });
                if still && cx.has_active_drag() {
                    this.open_dir(&wait, cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.spring = Some((folder, bounds, Some(task)));
    }

    /// The pointer moved with rows dragged over the tree area: near the top or the bottom edge
    /// the tree starts scrolling.
    pub(super) fn drag_near_edge(
        &mut self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let speed = if bounds.contains(&position) {
            edge_scroll(
                f32::from(position.y - bounds.origin.y),
                f32::from(bounds.size.height),
            )
        } else {
            0.0
        };
        self.edge_speed = speed;
        if speed == 0.0 || self.autoscroll.is_some() {
            return;
        }
        self.autoscroll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_EVERY).await;
                let go = this
                    .update_in(cx, |this, _, cx| {
                        if !cx.has_active_drag() || this.edge_speed == 0.0 {
                            this.autoscroll = None;
                            this.edge_speed = 0.0;
                            return false;
                        }
                        this.scroll_by(this.edge_speed);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        }));
    }

    /// Scrolls the tree `delta` pixels down (up when negative), inside what there is to see.
    pub fn scroll_by(&self, delta: f32) {
        let handle = self.scroll.0.borrow().base_handle.clone();
        let mut offset = handle.offset();
        let max = handle.max_offset().y;
        let y = (f32::from(offset.y) - delta).clamp(-f32::from(max), 0.0);
        offset.y = gpui::px(y);
        handle.set_offset(offset);
    }

    /// Rows were dropped on the row of `target` (a folder, or a file: its folder), or on the
    /// empty part of the tree (`None`: the root). ⌥ held copies instead of moving. The first
    /// drop of a session asks first.
    pub fn dropped(
        &mut self,
        rows: &DraggedRows,
        target: Option<(&Path, bool)>,
        copy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.spring = None;
        self.edge_speed = 0.0;
        let Some(dir) = target
            .and_then(|(path, is_dir)| drop_folder(path, is_dir))
            .or_else(|| self.tree.as_ref().map(|t| t.root().to_owned()))
        else {
            return;
        };
        let items: Vec<PathBuf> = if copy {
            rows.paths.clone()
        } else {
            rows.paths
                .iter()
                .filter(|p| !pointless_move(p, &dir))
                .cloned()
                .collect()
        };
        if items.is_empty() {
            return;
        }
        let confirm =
            self.env.settings.get::<FilesPanelSettings>().confirm_drag && !self.drag_confirmed;
        if confirm {
            window.dispatch_action(
                Box::new(FilesAskMove(FilesMove {
                    items: items.iter().map(|p| p.display().to_string()).collect(),
                    dir: dir.display().to_string(),
                    copy,
                })),
                cx,
            );
            return;
        }
        self.run_drop(items, dir, copy, window, cx);
    }

    /// The sheet of the first drop was confirmed: it is not asked again this session.
    pub fn move_confirmed(&mut self, ask: &FilesMove, window: &mut Window, cx: &mut Context<Self>) {
        self.drag_confirmed = true;
        let items = ask.items.iter().map(PathBuf::from).collect();
        self.run_drop(items, PathBuf::from(&ask.dir), ask.copy, window, cx);
    }

    fn run_drop(
        &mut self,
        items: Vec<PathBuf>,
        dir: PathBuf,
        copy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What lands there is seen: the folder opens.
        if self.tree.as_ref().is_some_and(|t| !t.is_expanded(&dir)) {
            self.open_dir(&dir, cx);
        }
        if copy {
            self.copy_into(items, dir, window, cx);
        } else {
            self.move_into(items, dir, window, cx);
        }
    }
}
