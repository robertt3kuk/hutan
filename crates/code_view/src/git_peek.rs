//! The git change marks of a code view (EDITOR-POLISH): a click on a mark opens the inline
//! **peek** of the hunk (what the lines were, *Revert hunk*, *Stage hunk*), ⌥F5 / ⇧⌥F5 (Vim's
//! `]c` / `[c`) step from change to change, and *Revert hunk* puts the base's lines back as one
//! undo step. Staging is the owner's: the view only says which lines (`GitHunkStage`).
//!
//! The marks themselves are drawn by `element.rs` from the buffer's hunks
//! ([`FileBuffer::git_hunks`](crate::FileBuffer::git_hunks)).

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Pixels, Point, StatefulInteractiveElement as _, Styled as _, anchored, deferred, div, point,
    px,
};

use crate::agent::HunkKind;
use crate::style::rgba;
use crate::view::{CodeView, CodeViewEvent};

/// Lines of the base's text the peek shows before it cuts.
const PEEK_LINES: usize = 12;

/// The peek while it shows: which hunk of the buffer's git hunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GitPeek {
    pub index: usize,
}

impl CodeView {
    /// Whether the owner can stage a hunk (the peek then has *Stage hunk*).
    pub fn set_git_stage_available(&mut self, available: bool, cx: &mut Context<Self>) {
        if self.git_stage != available {
            self.git_stage = available;
            cx.notify();
        }
    }

    /// Whether the peek offers *Stage hunk*.
    pub fn git_stage_available(&self) -> bool {
        self.git_stage
    }

    /// The git hunk the peek shows, if it is open.
    pub fn git_peek(&self) -> Option<usize> {
        self.git_peek.map(|p| p.index)
    }

    /// The git hunk the primary caret is in.
    pub fn git_hunk_at_caret(&self, cx: &gpui::App) -> Option<usize> {
        let buffer = self.buffer().read(cx);
        let head = self.selections.primary().head;
        let line = buffer.text().line_of_byte(head).ok()?;
        buffer.git_hunk_at_line(line)
    }

    /// The line (0-based) of the git mark under window position `position`: it is over the
    /// narrow strip at the right edge of the gutter, on a line that has a mark.
    pub(crate) fn git_mark_line_at(
        &self,
        position: Point<Pixels>,
        cx: &gpui::App,
    ) -> Option<usize> {
        let layout = self.layout?;
        let right = layout.text_left - layout.cell_width * 0.5;
        if position.x > right || position.x < right - layout.cell_width * 1.1 {
            return None;
        }
        if !layout
            .bounds
            .contains(&point(layout.bounds.left() + px(1.0), position.y))
        {
            return None;
        }
        let y = f32::from(position.y - layout.bounds.top()) + self.scroll_y;
        let row = (y / f32::from(layout.line_height)).floor();
        if row < 0.0 || row as usize >= self.display_rows(cx) {
            return None;
        }
        let (line, _) = self.line_of_row(row as usize, cx);
        self.buffer().read(cx).git_hunk_at_line(line).map(|_| line)
    }

    /// A click on a git mark: opens the peek of its hunk, or closes it when it is the open one.
    pub(crate) fn click_git_mark(&mut self, line: usize, cx: &mut Context<Self>) {
        let Some(index) = self.buffer().read(cx).git_hunk_at_line(line) else {
            return;
        };
        let open = self.git_peek();
        self.dismiss_hover(cx);
        if open == Some(index) {
            self.git_peek = None;
        } else {
            self.git_peek = Some(GitPeek { index });
        }
        cx.notify();
    }

    /// Opens the peek of the hunk at the caret (*Editor: Show Change*).
    pub fn show_git_hunk_at_caret(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.git_hunk_at_caret(cx) {
            self.git_peek = Some(GitPeek { index });
            cx.notify();
        }
    }

    /// *Editor: Show Change*: opens the peek of the change at the caret, or closes it.
    pub fn toggle_git_peek(&mut self, cx: &mut Context<Self>) {
        if !self.close_git_peek(cx) {
            self.show_git_hunk_at_caret(cx);
        }
    }

    /// Closes the peek. Returns whether it was open.
    pub fn close_git_peek(&mut self, cx: &mut Context<Self>) -> bool {
        if self.git_peek.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    /// The caret goes to the start of the next change against the index (wrapping around) and
    /// its peek opens.
    pub fn next_git_hunk(&mut self, cx: &mut Context<Self>) {
        self.step_git_hunk(true, cx);
    }

    /// The caret goes to the start of the previous change (wrapping around).
    pub fn previous_git_hunk(&mut self, cx: &mut Context<Self>) {
        self.step_git_hunk(false, cx);
    }

    pub(crate) fn step_git_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (hunks, text) = {
            let buffer = self.buffer().read(cx);
            (buffer.git_hunks(), buffer.text().clone())
        };
        if hunks.is_empty() {
            return;
        }
        // The line each hunk starts at: the stepping works on lines, so a caret anywhere in a
        // hunk is "in" it and the next press leaves it.
        let line_of = |at: usize| text.line_of_byte(text.floor_char_boundary(at.min(text.len())));
        let starts: Vec<usize> = hunks
            .iter()
            .map(|h| line_of(h.bytes.start).unwrap_or(0))
            .collect();
        let head_line = line_of(self.selections.primary().head).unwrap_or(0);
        let index = if forward {
            starts.iter().position(|&s| s > head_line).unwrap_or(0)
        } else {
            starts
                .iter()
                .rposition(|&s| s < head_line)
                .unwrap_or(starts.len() - 1)
        };
        self.buffer().update(cx, |b, _| b.break_group());
        self.reveal_line(starts[index] + 1, None, cx);
        self.git_peek = Some(GitPeek { index });
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// *Revert hunk*: the base's text goes back in place of git hunk `index` as one undo step
    /// (⌘Z brings the change and this view's carets back). Returns whether it did.
    pub fn revert_git_hunk(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let id = cx.entity_id();
        let before = self.selections.clone();
        let applied = self
            .buffer()
            .update(cx, |b, cx| b.revert_git_hunk(index, id, Some(before), cx));
        let Some(applied) = applied else {
            return false;
        };
        self.git_peek = None;
        let text = self.buffer().read(cx).text().clone();
        if let Some(after) = applied.transaction.selection_after() {
            self.selections = after.clamp(&text);
        }
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        true
    }

    /// *Revert hunk* on the hunk at the caret (the peek's, when it is open).
    pub fn revert_git_hunk_at_caret(&mut self, cx: &mut Context<Self>) {
        let index = self.git_peek().or_else(|| self.git_hunk_at_caret(cx));
        if let Some(index) = index {
            self.revert_git_hunk(index, cx);
        }
    }

    /// *Stage hunk* on hunk `index`: asks the owner to stage its lines.
    pub fn stage_git_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((lines, kind)) = self.git_hunk_lines(index, cx) else {
            return;
        };
        self.git_peek = None;
        cx.emit(CodeViewEvent::GitHunkStage {
            first_line: lines.start,
            end_line: lines.end,
            kind,
        });
        cx.notify();
    }

    /// *Stage hunk* on the hunk at the caret (the peek's, when it is open).
    pub fn stage_git_hunk_at_caret(&mut self, cx: &mut Context<Self>) {
        let index = self.git_peek().or_else(|| self.git_hunk_at_caret(cx));
        if let Some(index) = index {
            self.stage_git_hunk(index, cx);
        }
    }

    /// The lines (0-based) of git hunk `index`: empty for removed lines, then at the line the
    /// mark is drawn above.
    pub fn git_hunk_lines(&self, index: usize, cx: &gpui::App) -> Option<(Range<usize>, HunkKind)> {
        let buffer = self.buffer().read(cx);
        let hunk = buffer.git_hunks().get(index)?.clone();
        let text = buffer.text();
        let at = |byte: usize| text.line_of_byte(text.floor_char_boundary(byte.min(text.len())));
        let start = at(hunk.bytes.start).ok()?;
        let kind = hunk.kind();
        if kind == HunkKind::Removed {
            return Some((start..start, kind));
        }
        let last = at(hunk.bytes.end.saturating_sub(1).max(hunk.bytes.start)).ok()?;
        Some((start..last + 1, kind))
    }

    /// The peek: the change's kind, what the lines were and its buttons, below the hunk.
    pub(crate) fn render_git_peek(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let peek = self.git_peek?;
        let layout = self.layout?;
        let (hunk, kind_name, count) = {
            let buffer = self.buffer().read(cx);
            let hunk = buffer.git_hunks().get(peek.index)?.clone();
            let base = buffer.git_base()?.kind;
            (hunk, base.name(), buffer.git_hunks().len())
        };
        let (lines, kind) = self.git_hunk_lines(peek.index, cx)?;
        let theme = &self.style.theme;
        let fg = rgba(theme.foreground);
        let muted = rgba(theme.gutter_foreground);
        let color = rgba(match kind {
            HunkKind::Added => theme.diff_added,
            HunkKind::Modified => theme.diff_modified,
            HunkKind::Removed => theme.diff_removed,
        });
        let title = match kind {
            HunkKind::Added => format!("Added {} {}", lines.len(), plural(lines.len())),
            HunkKind::Modified => format!("Changed {} {}", lines.len(), plural(lines.len())),
            HunkKind::Removed => {
                let removed = hunk.base_text.lines().count();
                format!("Removed {removed} {}", plural(removed))
            }
        };
        // Below the hunk's last row (the line the mark is above, for removed lines).
        let last_line = if kind == HunkKind::Removed {
            lines.start
        } else {
            lines.end - 1
        };
        let row = {
            let buffer = self.buffer().read(cx);
            let text = buffer.text();
            let end = text.line_content_range(last_line).map_or(0, |r| r.end);
            self.row_of_offset(end, cx)
        };
        let y = layout.bounds.top() + layout.line_height * (row as f32 + 1.0) - px(self.scroll_y);
        let x = layout.text_left - layout.cell_width * 0.5;
        // As wide as its text, but inside the view: a narrow pane's peek never reaches into its
        // neighbour.
        let cell = f32::from(layout.cell_width);
        let room = f32::from(layout.bounds.right() - x);
        let max_w = room.clamp(cell * 24.0, cell * 80.0);
        let min_w = (cell * 36.0).min(max_w);
        let mut border = muted;
        border.a *= 0.35;
        let stage = self.git_stage;
        let editable = self.buffer().read(cx).is_editable();
        let index = peek.index;
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(|s| s.bg(rgba(theme.selection)))
                .child(label)
        };
        let mut body =
            self.popup_surface()
                .id("code-git-peek")
                .flex()
                .flex_col()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .min_w(px(min_w))
                .max_w(px(max_w))
                .shadow_md()
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().flex_none().w(px(3.0)).h(layout.line_height).bg(color))
                        .child(
                            div().flex_1().min_w_0().child(div().child(title)).child(
                                div().text_color(muted).child(format!(
                                    "against {kind_name} · {} of {count}",
                                    index + 1
                                )),
                            ),
                        )
                        .when(stage, |row| {
                            row.child(button("git-peek-stage", "Stage hunk").on_click(
                                cx.listener(move |this, _, _, cx| this.stage_git_hunk(index, cx)),
                            ))
                        })
                        .when(editable, |row| {
                            row.child(button("git-peek-revert", "Revert hunk").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.revert_git_hunk(index, cx);
                                }),
                            ))
                        })
                        .child(button("git-peek-close", "✕").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.close_git_peek(cx);
                            },
                        ))),
                );
        if kind != HunkKind::Added {
            let shown: Vec<&str> = hunk.base_text.lines().take(PEEK_LINES).collect();
            let more = hunk.base_text.lines().count().saturating_sub(PEEK_LINES);
            let mut removed = rgba(theme.diff_removed);
            removed.a *= 0.18;
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .font_family(self.style.font_family.clone())
                    .text_color(fg)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .children(
                        shown
                            .into_iter()
                            .map(|line| div().bg(removed).px(px(4.0)).child(format!("- {line}"))),
                    )
                    .when(more > 0, |s| {
                        s.child(div().text_color(muted).child(format!("… {more} more")))
                    }),
            );
        }
        Some(
            deferred(anchored().position(point(x, y)).child(body))
                .with_priority(1)
                .into_any_element(),
        )
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "line" } else { "lines" }
}
