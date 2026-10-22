//! Line selection, review comments and the Review tray of the Changes view (RC-1): click a line
//! number (drag, or shift-click) to select lines of one hunk, write a comment under them, and
//! send the review to the agent that works in the checkout.
//!
//! Comments are drafts held by the desktop (saved in `desktop-state.json` by the workbench, which
//! hears of every change through [`ChangesEvent::Comments`]); a send is one chat message with
//! R-1 attachments, or a bracketed paste into the terminal of an agent that has no chat. See
//! [`super::comments`] for what a comment is and how it is written down.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_chat_view::diff::{DiffLine, FileDiff, LineKind};
use hutan_core::SessionId;

use super::comments::{
    self, ComposeError, MAX_COMMENTS, Mark, ReviewComment, SnippetLine, clock, compose_chat,
    compose_plain, diff_text, now_ms, snippet,
};
use super::{ChangesEvent, ChangesView, FilePatch, hunk_ranges};
use crate::settings::{ChatSettings, SendKey};
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::vim::{Mode, Outcome, Vim};
use crate::workbench::chat::{Look, TextInput, diff_row};
use crate::workbench::panels::{age_label, edit_text, field_box};
use crate::workbench::render::probe;
use crate::workbench::text_field::{FieldMut, FieldRef};
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, VimConfig};

/// The UI font, for the cards drawn inside the diff's monospace block.
const UI_FONT: &str = ".SystemUIFont";

/// Lines of one hunk of the shown file's diff, as indices into its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub file: String,
    pub hunk: usize,
    /// Where the selection began (a click, or the first of a drag).
    pub anchor: usize,
    /// Where it reaches now.
    pub head: usize,
}

impl Selection {
    /// The first and the last selected line.
    pub fn span(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// The comment box: a new comment on a selection, or the edit of one.
#[derive(Debug)]
struct Draft {
    file: String,
    hunk: usize,
    from: usize,
    to: usize,
    editing: Option<u64>,
    input: TextInput,
    vim: Vim,
    /// What the selected lines said when the box opened, and their surroundings: where the
    /// lines are found again when the patch is read anew, and what the comment is saved with.
    lines: Vec<SnippetLine>,
    excerpt: Vec<SnippetLine>,
    /// The lines are gone from the patch (the file changed under the box).
    outdated: bool,
}

/// An agent a review can go to: a live chat, or a terminal with an agent, in this checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendTarget {
    pub session: SessionId,
    pub title: String,
    /// A chat (the review goes as a message with attachments); else a terminal (as a paste).
    pub chat: bool,
}

/// Everything the Review part of a Changes view holds.
#[derive(Debug)]
pub(super) struct Review {
    /// Every comment of this checkout, over every base it was reviewed against.
    pub comments: Vec<ReviewComment>,
    /// Where each comment of the current base is in its file's patch now (first and last
    /// line index), for the patches that were read.
    pub(super) placed: HashMap<u64, (usize, usize)>,
    pub(super) selection: Option<Selection>,
    dragging: bool,
    draft: Option<Draft>,
    /// The comment `]c` / `[c` or a click in the tray went to last.
    focused: Option<u64>,
    /// `]` or `[` was pressed: a `c` next makes the sequence.
    bracket: Option<char>,
    menu_open: bool,
    list_open: bool,
    /// Sent batches (by their time) the tray shows open.
    sent_open: HashSet<u64>,
    notice: Option<String>,
    sending: bool,
    pub focus: FocusHandle,
    comment_focus: FocusHandle,
    pub scroll: ScrollHandle,
    /// A comment to scroll into view once it is drawn.
    reveal: Rc<Cell<Option<u64>>>,
}

impl Review {
    pub(super) fn new(comments: Vec<ReviewComment>, cx: &mut Context<ChangesView>) -> Self {
        Self {
            comments,
            placed: HashMap::new(),
            selection: None,
            dragging: false,
            draft: None,
            focused: None,
            bracket: None,
            menu_open: false,
            list_open: true,
            sent_open: HashSet::new(),
            notice: None,
            sending: false,
            focus: cx.focus_handle(),
            comment_focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            reveal: Rc::new(Cell::new(None)),
        }
    }
}

/// Where the comments of the shown file are drawn.
pub(super) struct Anchors {
    /// Comment ids to draw under each diff line.
    last: HashMap<usize, Vec<u64>>,
    /// Position of each comment in [`ChangesView::comments`] (what regions count by).
    index: HashMap<u64, usize>,
}

/// Where `lines` are in `diff` now: the hunk and the first and last line index of the closest
/// run of lines saying the same, `None` when the lines are gone.
fn place(
    diff: &FileDiff,
    ranges: &[std::ops::Range<usize>],
    lines: &[SnippetLine],
) -> Option<(usize, usize, usize)> {
    let hint = lines.first().and_then(|l| l.new.or(l.old));
    let (first, last) = comments::locate(diff, ranges, lines, hint)?;
    let hunk = ranges.iter().position(|r| r.contains(&first))?;
    Some((hunk, first, last))
}

impl ChangesView {
    // ---- read access (also for tests) -----------------------------------------------------

    /// The comments of this diff (this checkout against the base listed), in the order the tray
    /// shows them: by file, then by line.
    pub fn comments(&self) -> Vec<ReviewComment> {
        self.ordered().into_iter().cloned().collect()
    }

    /// The selected lines of the shown file: first and last index into its diff lines.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.review.selection.as_ref().map(Selection::span)
    }

    /// Whether the comment box is open.
    pub fn comment_box_open(&self) -> bool {
        self.review.draft.is_some()
    }

    /// What is typed in the open comment box of a new comment.
    pub fn comment_text(&self) -> Option<String> {
        self.review
            .draft
            .as_ref()
            .filter(|d| d.editing.is_none())
            .map(|d| d.input.text().to_owned())
    }

    /// Where the open comment box of a new comment sits: the last diff line it is under, and
    /// whether its lines are gone from the patch.
    pub fn comment_box_at(&self) -> Option<(usize, bool)> {
        self.review
            .draft
            .as_ref()
            .filter(|d| d.editing.is_none())
            .map(|d| (d.to, d.outdated))
    }

    /// What the tray says about a send that did not go.
    pub fn review_notice(&self) -> Option<&str> {
        self.review.notice.as_deref()
    }

    /// The comment the last `]c`/`[c` (or a tray row) went to.
    pub fn focused_comment(&self) -> Option<u64> {
        self.review.focused
    }

    /// Where comment `id` is in the shown patch: first and last diff line.
    pub fn comment_lines(&self, id: u64) -> Option<(usize, usize)> {
        self.review.placed.get(&id).copied()
    }

    pub fn review_menu_open(&self) -> bool {
        self.review.menu_open
    }

    pub fn review_sending(&self) -> bool {
        self.review.sending
    }

    /// Whether the view takes the review's keys (`c`, `]c`, `[c`, ⌘↩).
    pub fn review_focused(&self, window: &Window) -> bool {
        self.review.focus.is_focused(window)
    }

    /// Gives the view the keyboard.
    pub fn focus_review(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.review.focus, cx);
    }

    /// Where a review can go: the live agent chats and terminals with an agent in this
    /// checkout. The first is the default: the one a task of this checkout runs, then chats
    /// before terminals, then the newest.
    pub fn send_targets(&self, cx: &gpui::App) -> Vec<SendTarget> {
        let store = self.env.sessions.read(cx);
        let linked: HashSet<SessionId> = store
            .tasks()
            .ordered()
            .into_iter()
            .flat_map(|task| task.sessions.iter().copied())
            .collect();
        let mut found: Vec<(bool, bool, u64, SendTarget)> = store
            .sessions()
            .iter()
            .filter(|s| {
                s.state.is_live()
                    && s.placement
                        .as_ref()
                        .is_some_and(|p| p.root == self.path || p.root == self.canonical)
            })
            .filter_map(|s| {
                let chat = match &s.chat {
                    Some(chat) if chat.is_assistant() => return None,
                    Some(_) => true,
                    None if s.terminal_id.is_some() && s.agent.is_some() => false,
                    None => return None,
                };
                Some((
                    linked.contains(&s.id),
                    chat,
                    s.created_at_ms,
                    SendTarget {
                        session: s.id,
                        title: s.title.clone(),
                        chat,
                    },
                ))
            })
            .collect();
        found.sort_by_key(|a| std::cmp::Reverse((a.0, a.1, a.2)));
        found.into_iter().map(|(_, _, _, target)| target).collect()
    }

    fn ordered(&self) -> Vec<&ReviewComment> {
        let base = self.base();
        let position = |file: &str| {
            self.files()
                .iter()
                .position(|f| f.path == file)
                .unwrap_or(usize::MAX)
        };
        let mut list: Vec<&ReviewComment> = self
            .review
            .comments
            .iter()
            .filter(|c| Some(c.base.as_str()) == base)
            .collect();
        list.sort_by_key(|c| {
            (
                position(&c.file),
                c.file.as_str(),
                c.hint().unwrap_or(0),
                c.id,
            )
        });
        list
    }

    fn comment(&self, id: u64) -> Option<&ReviewComment> {
        self.review.comments.iter().find(|c| c.id == id)
    }

    fn comment_mut(&mut self, id: u64) -> Option<&mut ReviewComment> {
        self.review.comments.iter_mut().find(|c| c.id == id)
    }

    fn drafts(&self) -> Vec<u64> {
        self.ordered()
            .into_iter()
            .filter(|c| c.is_draft())
            .map(|c| c.id)
            .collect()
    }

    /// Tells the workbench to save the comments.
    fn comments_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ChangesEvent::Comments(self.review.comments.clone()));
        cx.notify();
    }

    // ---- selecting lines ------------------------------------------------------------------

    /// Selects diff line `line` of the shown file (a click on its number); with `extend`
    /// (shift) the selection grows from where it began, within the hunk. Lines that are not
    /// content (hunk headers) select nothing.
    pub fn select_line(&mut self, line: usize, extend: bool, cx: &mut Context<Self>) {
        let (Some(file), Some(diff)) = (self.selected.clone(), self.selected_diff()) else {
            return;
        };
        let Some((hunk, _)) = hunk_ranges(diff)
            .into_iter()
            .enumerate()
            .find(|(_, r)| r.start < line && line < r.end)
        else {
            return;
        };
        if diff
            .lines
            .get(line)
            .is_none_or(|l| matches!(l.kind, LineKind::Hunk | LineKind::Note))
        {
            return;
        }
        let anchor = match &self.review.selection {
            Some(s) if extend && s.file == file && s.hunk == hunk => s.anchor,
            _ => line,
        };
        self.review.selection = Some(Selection {
            file,
            hunk,
            anchor,
            head: line,
        });
        cx.notify();
    }

    /// A drag over the numbers reaches `line`.
    pub fn drag_to(&mut self, line: usize, cx: &mut Context<Self>) {
        let Some(selection) = &self.review.selection else {
            return;
        };
        let hunk = selection.hunk;
        let Some(diff) = self.selected_diff() else {
            return;
        };
        let inside = hunk_ranges(diff)
            .get(hunk)
            .is_some_and(|r| r.start < line && line < r.end);
        if inside
            && diff
                .lines
                .get(line)
                .is_some_and(|l| !matches!(l.kind, LineKind::Hunk | LineKind::Note))
            && let Some(selection) = &mut self.review.selection
            && selection.head != line
        {
            // Only the head moves: the selection began where it did.
            selection.head = line;
            cx.notify();
        }
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.review.selection.take().is_some() {
            cx.notify();
        }
    }

    // ---- the comment box ------------------------------------------------------------------

    /// "+ Comment" (or `c`): opens the comment box under the selected lines. False without a
    /// selection.
    pub fn open_comment_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.review.selection.clone() else {
            return false;
        };
        let (from, to) = selection.span();
        let Some((lines, excerpt)) = self.selected_diff().and_then(|diff| {
            hunk_ranges(diff)
                .get(selection.hunk)
                .and_then(|hunk| snippet(diff, hunk, from, to))
        }) else {
            return false;
        };
        self.review.draft = Some(Draft {
            file: selection.file,
            hunk: selection.hunk,
            from,
            to,
            editing: None,
            input: TextInput::new(),
            vim: Vim::new(),
            lines,
            excerpt,
            outdated: false,
        });
        self.review.notice = None;
        window.focus(&self.review.comment_focus, cx);
        cx.notify();
        true
    }

    /// The text of the open comment box.
    pub fn set_comment_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(draft) = &mut self.review.draft {
            draft.input.set(text);
            cx.notify();
        }
    }

    /// ⌘↩ in the comment box: adds the comment (or saves the edit). False when the text is
    /// empty, the box is not open, or the diff holds as many comments as it may.
    pub fn add_comment(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(draft) = &self.review.draft else {
            return false;
        };
        let text = draft.input.text().trim().to_owned();
        if text.is_empty() {
            return false;
        }
        if let Some(id) = draft.editing {
            self.review.draft = None;
            if let Some(comment) = self.comment_mut(id) {
                comment.text = text;
            }
            self.comments_changed(cx);
            return true;
        }
        let (Some(base), Some(FilePatch::Ready { diff, .. })) = (
            self.base().map(str::to_owned),
            self.patches.get(&draft.file),
        ) else {
            return false;
        };
        if self.ordered().len() >= MAX_COMMENTS {
            self.review.notice = Some(format!(
                "A diff keeps at most {MAX_COMMENTS} comments: delete some first"
            ));
            cx.notify();
            return false;
        }
        let ranges = hunk_ranges(diff);
        let (lines, excerpt) = (draft.lines.clone(), draft.excerpt.clone());
        if lines.is_empty() {
            return false;
        }
        let id = self
            .review
            .comments
            .iter()
            .map(|c| c.id)
            .max()
            .map_or(1, |m| m + 1);
        let mut comment = ReviewComment {
            id,
            base,
            file: draft.file.clone(),
            lines,
            excerpt,
            text,
            created_ms: now_ms(),
            sent_ms: None,
            sent_to: None,
            resolved: false,
            keep_open: false,
            outdated: false,
        };
        if draft.outdated {
            comment.outdated = true;
        } else if let Some(found) = comments::locate(diff, &ranges, &comment.lines, comment.hint())
        {
            self.review.placed.insert(id, found);
        } else {
            comment.outdated = true;
        }
        self.review.comments.push(comment);
        self.review.draft = None;
        self.review.selection = None;
        self.review.focused = Some(id);
        self.comments_changed(cx);
        true
    }

    /// Esc in the comment box.
    pub fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        if self.review.draft.take().is_some() {
            cx.notify();
        }
    }

    /// "Edit" on a comment: its text in the box.
    pub fn edit_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.comment(id) else {
            return;
        };
        let mut input = TextInput::new();
        input.set(&comment.text);
        let (file, from, to) = (comment.file.clone(), 0, 0);
        self.review.draft = Some(Draft {
            file,
            hunk: 0,
            from,
            to,
            editing: Some(id),
            input,
            vim: Vim::new(),
            lines: Vec::new(),
            excerpt: Vec::new(),
            outdated: false,
        });
        window.focus(&self.review.comment_focus, cx);
        cx.notify();
    }

    pub fn delete_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.review.comments.len();
        self.review.comments.retain(|c| c.id != id);
        if self.review.comments.len() == before {
            return;
        }
        self.review.placed.remove(&id);
        if self.review.focused == Some(id) {
            self.review.focused = None;
        }
        if self
            .review
            .draft
            .as_ref()
            .is_some_and(|d| d.editing == Some(id))
        {
            self.review.draft = None;
        }
        self.comments_changed(cx);
    }

    /// Resolve / Unresolve. A reopened comment stays open when its lines change.
    pub fn set_resolved(&mut self, id: u64, resolved: bool, cx: &mut Context<Self>) {
        if let Some(comment) = self.comment_mut(id)
            && comment.resolved != resolved
        {
            comment.resolved = resolved;
            comment.keep_open = !resolved;
            self.comments_changed(cx);
        }
    }

    // ---- finding the comments in the patches ----------------------------------------------

    /// A reload begins: the patches are read again, so what pointed into them does not.
    pub(super) fn review_reloading(&mut self) {
        self.review.placed.clear();
        self.review.selection = None;
        if self
            .review
            .draft
            .as_ref()
            .is_some_and(|d| d.editing.is_none())
        {
            self.review.draft = None;
        }
    }

    /// After a listing: loads the patches of the files with comments (their comments are placed
    /// as the patches arrive) and settles the comments of files that are not changed any more.
    pub(super) fn review_listed(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.base().map(str::to_owned) else {
            return;
        };
        let listed: HashSet<String> = self.files().iter().map(|f| f.path.clone()).collect();
        let mut wanted: Vec<String> = Vec::new();
        let mut changed = false;
        let resolve = self.include_uncommitted;
        for comment in self.review.comments.iter_mut().filter(|c| c.base == base) {
            if listed.contains(&comment.file) {
                if !wanted.contains(&comment.file) {
                    wanted.push(comment.file.clone());
                }
            } else {
                self.review.placed.remove(&comment.id);
                changed |= settle(comment, false, resolve);
            }
        }
        for file in wanted {
            self.load_patch(&file, cx);
        }
        if changed {
            self.comments_changed(cx);
        }
    }

    /// The patch of `file` was replaced by one that may differ from `before` (a refresh): the
    /// line selection and the open comment box follow their lines to where those are now. A
    /// selection whose lines are gone is dropped; a box whose lines are gone stays, with its
    /// text, and says so.
    pub(super) fn review_patch_replaced(&mut self, file: &str, before: &FileDiff) {
        let Some(FilePatch::Ready { diff: now, .. }) = self.patches.get(file) else {
            return;
        };
        if now == before {
            return;
        }
        let ranges = hunk_ranges(now);
        if let Some(selection) = self.review.selection.clone()
            && selection.file == file
        {
            let (from, to) = selection.span();
            let moved = hunk_ranges(before)
                .get(selection.hunk)
                .and_then(|hunk| snippet(before, hunk, from, to))
                .and_then(|(lines, _)| place(now, &ranges, &lines));
            self.review.selection = moved.map(|(hunk, first, last)| Selection {
                file: selection.file,
                hunk,
                anchor: first,
                head: last,
            });
        }
        if let Some(draft) = &mut self.review.draft
            && draft.editing.is_none()
            && draft.file == file
        {
            match place(now, &ranges, &draft.lines) {
                Some((hunk, first, last)) => {
                    draft.hunk = hunk;
                    draft.from = first;
                    draft.to = last;
                    draft.outdated = false;
                }
                None => {
                    draft.outdated = true;
                    // The box stays where it can still be drawn.
                    draft.to = draft.to.min(now.lines.len().saturating_sub(1));
                    draft.from = draft.from.min(draft.to);
                }
            }
        }
    }

    /// The patch of `file` arrived: finds each comment on it again — where the lines moved to,
    /// or gone (outdated). A sent comment whose lines changed is resolved.
    pub(super) fn review_patch_arrived(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(base) = self.base().map(str::to_owned) else {
            return;
        };
        let Some(FilePatch::Ready { diff, .. }) = self.patches.get(file) else {
            return;
        };
        let ranges = hunk_ranges(diff);
        let resolve = self.include_uncommitted;
        let mut changed = false;
        for comment in self
            .review
            .comments
            .iter_mut()
            .filter(|c| c.file == file && c.base == base)
        {
            match comments::locate(diff, &ranges, &comment.lines, comment.hint()) {
                Some(found) => {
                    self.review.placed.insert(comment.id, found);
                    changed |= comment.renumber(diff, found);
                    changed |= settle(comment, true, resolve);
                }
                None => {
                    self.review.placed.remove(&comment.id);
                    changed |= settle(comment, false, resolve);
                }
            }
        }
        if changed {
            self.comments_changed(cx);
        }
    }

    // ---- sending --------------------------------------------------------------------------

    /// "Send to agent": every draft of this diff, as one message to `target` (the default
    /// target when `None`).
    pub fn send_review(&mut self, target: Option<SessionId>, cx: &mut Context<Self>) {
        let ids = self.drafts();
        self.send_ids(ids, target, cx);
    }

    /// "Send now" on one comment, to the default target.
    pub fn send_comment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.send_ids(vec![id], None, cx);
    }

    fn send_ids(&mut self, ids: Vec<u64>, target: Option<SessionId>, cx: &mut Context<Self>) {
        if self.review.sending {
            return;
        }
        self.review.menu_open = false;
        let picked: Vec<ReviewComment> = ids
            .iter()
            .filter_map(|id| self.comment(*id).cloned())
            .collect();
        if picked.is_empty() {
            self.review.notice = Some("There are no comments to send".to_owned());
            cx.notify();
            return;
        }
        let targets = self.send_targets(cx);
        let Some(target) = target
            .and_then(|wanted| targets.iter().find(|t| t.session == wanted))
            .or_else(|| targets.first())
            .cloned()
        else {
            self.review.notice = Some(
                "No agent works in this checkout: use ▾ to start one with these comments"
                    .to_owned(),
            );
            cx.notify();
            return;
        };
        let refs: Vec<&ReviewComment> = picked.iter().collect();
        let sending = if target.chat {
            compose_chat(&self.path, &refs).map(|composed| {
                self.env.sessions.update(cx, |store, cx| {
                    store.chat_send(target.session, composed.text, composed.attachments, cx)
                })
            })
        } else {
            compose_plain(&refs).map(|text| {
                self.env.sessions.update(cx, |store, cx| {
                    store.paste_into_terminal(target.session, &text, cx)
                })
            })
        };
        let task = match sending {
            Ok(task) => task,
            Err(error) => {
                self.review.notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.review.sending = true;
        self.review.notice = None;
        let count = picked.len();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.review.sending = false;
                match result {
                    Ok(()) => {
                        this.mark_sent(&ids, &target.title);
                        cx.emit(ChangesEvent::Notice(format!(
                            "Sent {count} review comment{} to {}",
                            if count == 1 { "" } else { "s" },
                            target.title
                        )));
                        this.comments_changed(cx);
                    }
                    Err(error) => {
                        this.review.notice = Some(format!("Could not send: {error}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// "Start a new agent with these comments": the launcher opens on this checkout with the
    /// review as its prompt. The comments stay drafts until the new agent has the message
    /// ([`ChangesView::comments_sent`]).
    pub fn start_new_agent(&mut self, cx: &mut Context<Self>) {
        self.review.menu_open = false;
        let ids = self.drafts();
        let picked: Vec<&ReviewComment> = ids.iter().filter_map(|id| self.comment(*id)).collect();
        match compose_plain(&picked) {
            Ok(text) => {
                cx.emit(ChangesEvent::StartAgent {
                    cwd: self.path.clone(),
                    text,
                    comments: ids,
                });
                cx.notify();
            }
            Err(ComposeError::Empty) => {
                self.review.notice = Some("There are no comments to send".to_owned());
                cx.notify();
            }
            Err(error) => {
                self.review.notice = Some(error.to_string());
                cx.notify();
            }
        }
    }

    /// The comments `ids` reached the agent `to` (a new agent the review started): they are
    /// sent now.
    pub fn comments_sent(&mut self, ids: &[u64], to: &str, cx: &mut Context<Self>) {
        self.mark_sent(ids, to);
        self.comments_changed(cx);
    }

    fn mark_sent(&mut self, ids: &[u64], to: &str) {
        let at = now_ms();
        for comment in self
            .review
            .comments
            .iter_mut()
            .filter(|c| ids.contains(&c.id))
        {
            comment.sent_ms = Some(at);
            comment.sent_to = Some(to.to_owned());
        }
    }

    // ---- moving between comments ----------------------------------------------------------

    /// `]c` / `[c`: the next or previous open comment, over every file.
    pub fn step_comment(&mut self, forward: bool, cx: &mut Context<Self>) {
        let open: Vec<u64> = self
            .ordered()
            .into_iter()
            .filter(|c| !c.resolved)
            .map(|c| c.id)
            .collect();
        if open.is_empty() {
            return;
        }
        let at = self
            .review
            .focused
            .and_then(|id| open.iter().position(|i| *i == id));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => open.len() - 1,
            (Some(i), true) => (i + 1) % open.len(),
            (Some(i), false) => (i + open.len() - 1) % open.len(),
        };
        self.jump_to(open[next], cx);
    }

    /// Shows comment `id`: its file, scrolled to it.
    pub fn jump_to(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(file) = self.comment(id).map(|c| c.file.clone()) else {
            return;
        };
        if self.selected.as_deref() != Some(file.as_str()) {
            self.review.selection = None;
            self.select(&file, cx);
        }
        self.review.focused = Some(id);
        self.review.reveal.set(Some(id));
        cx.notify();
    }

    // ---- keys -----------------------------------------------------------------------------

    /// The review's keys while the view has the keyboard: `c` opens the comment box on the
    /// selected lines, `]c` / `[c` go to the next / previous comment, ⌘↩ sends the review,
    /// Esc closes the menu or drops the selection.
    pub(super) fn on_review_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.review.focus.is_focused(window) {
            return;
        }
        let ks = &event.keystroke;
        let plain = !(ks.modifiers.platform
            || ks.modifiers.control
            || ks.modifiers.alt
            || ks.modifiers.function
            || ks.modifiers.shift);
        let pending = self.review.bracket.take();
        match ks.key.as_str() {
            "enter" if ks.modifiers.platform => self.send_review(None, cx),
            // Under `chat.send_key = "enter"` Return sends the review too, once there is one.
            "enter"
                if plain
                    && self.send_key().return_sends(false, false)
                    && !self.drafts().is_empty() =>
            {
                self.send_review(None, cx);
            }
            "c" if plain => match pending {
                Some(bracket) => self.step_comment(bracket == ']', cx),
                None => {
                    if !self.open_comment_box(window, cx) {
                        return;
                    }
                }
            },
            key @ ("[" | "]") if plain => {
                self.review.bracket = key.chars().next();
            }
            "escape" if self.review.menu_open => {
                self.review.menu_open = false;
                cx.notify();
            }
            "escape" if self.review.selection.is_some() => self.clear_selection(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The send key now in force (`chat.send_key`), read live.
    fn send_key(&self) -> SendKey {
        self.env.settings.get::<ChatSettings>().send_key
    }

    /// Return in the comment box (`chat.send_key`): adds the comment, or a line, as the setting
    /// says; ⇧Return and ⌥Return always add a line. A Return that commits an input method's
    /// marked text only commits it, and in Vim's Normal mode it is no newline.
    fn comment_return(
        &mut self,
        shift: bool,
        alt: bool,
        vim_on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sends = self.send_key().return_sends(shift, alt);
        if let Some(draft) = &mut self.review.draft {
            let normal = vim_on && draft.vim.mode() != Mode::Insert;
            if draft.input.marked().is_some() {
                draft.input.unmark();
            } else if sends {
                if self.add_comment(cx) {
                    window.focus(&self.review.focus, cx);
                }
                return;
            } else if !normal {
                draft.input.insert("\n");
            }
        }
        cx.notify();
    }

    fn on_comment_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vim = VimConfig::of(&self.env.settings);
        match event.keystroke.key.as_str() {
            "escape" => {
                if vim.enabled
                    && let Some(draft) = &mut self.review.draft
                    && vim_ui::apply(
                        &mut draft.vim,
                        &mut draft.input,
                        event,
                        vim.options(false, true),
                        cx,
                    ) == Outcome::Handled
                {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                self.cancel_comment(cx);
                window.focus(&self.review.focus, cx);
            }
            "enter" if event.keystroke.modifiers.platform => {
                if self.add_comment(cx) {
                    window.focus(&self.review.focus, cx);
                }
            }
            "enter" if !event.keystroke.modifiers.control => {
                let mods = &event.keystroke.modifiers;
                self.comment_return(mods.shift, mods.alt, vim.enabled, window, cx);
            }
            _ => {
                let Some(draft) = &mut self.review.draft else {
                    return;
                };
                if !edit_text(&mut draft.input, &mut draft.vim, vim, event, true, cx) {
                    return;
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// How the comment box's element reaches its text: the platform's text input and the mouse.
    fn comment_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.review.comment_focus.clone(),
            |this, _, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(draft) = &mut this.review.draft {
                    f(&mut FieldMut {
                        input: &mut draft.input,
                        vim: vim_on.then_some(&mut draft.vim),
                        multiline: true,
                    });
                }
            },
        )
    }

    // ---- drawing --------------------------------------------------------------------------

    /// What the tray says of the comments: `3 comments · 2 sent · 1 resolved` (the count is of
    /// drafts: what "Send to agent" sends).
    pub fn review_summary(&self) -> String {
        let ordered = self.ordered();
        let drafts = ordered.iter().filter(|c| c.is_draft()).count();
        let sent = ordered.iter().filter(|c| c.sent_ms.is_some()).count();
        let resolved = ordered.iter().filter(|c| c.resolved).count();
        let mut summary = format!("{drafts} comment{}", if drafts == 1 { "" } else { "s" });
        if sent > 0 {
            summary.push_str(&format!(" · {sent} sent"));
        }
        if resolved > 0 {
            summary.push_str(&format!(" · {resolved} resolved"));
        }
        summary
    }

    /// The Review tray: the count of comments with "Send to agent ▾", the menu of targets, and
    /// the list of drafts and sent reviews. Nothing while there are no comments.
    pub(super) fn render_review_tray(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ordered = self.ordered();
        if ordered.is_empty() && self.review.notice.is_none() {
            return None;
        }
        let drafts: Vec<&ReviewComment> =
            ordered.iter().copied().filter(|c| c.is_draft()).collect();
        let summary = self.review_summary();
        let targets = self.send_targets(cx);
        let secondary = tokens.secondary_font_size;
        let can_send = !drafts.is_empty() && !self.review.sending;
        let default_title = targets.first().map(|t| t.title.clone());
        let mut tray = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 1.5))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(div().medium().child("Review"))
                    .child(
                        div()
                            .id("changes-review-summary")
                            .relative()
                            .text_color(theme.muted)
                            .text_size(px(secondary))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review.list_open = !this.review.list_open;
                                cx.notify();
                            }))
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(crate::icons::glyph(if self.review.list_open {
                                crate::icons::Icon::ActionExpand
                            } else {
                                crate::icons::Icon::ActionDisclose
                            }))
                            .child(SharedString::from(summary))
                            .child(probe(&self.env.regions, Region::Changes("review-summary"))),
                    )
                    .child(div().flex_1())
                    .when_some(default_title, |s, title| {
                        s.child(
                            div()
                                .text_color(theme.muted)
                                .text_size(px(secondary))
                                .truncate()
                                .child(SharedString::from(format!("→ {title}"))),
                        )
                    })
                    .child(
                        ui::Button::new(
                            "changes-review-send",
                            "Send to agent",
                            theme,
                            tokens,
                        )
                        .primary()
                        .loading(self.review.sending)
                        .disabled(!can_send)
                        .on_click(cx.listener(|this, _, _, cx| this.send_review(None, cx)))
                        .child(probe(&self.env.regions, Region::Changes("review-send"))),
                    )
                    .child(
                        ui::IconButton::new(
                            "changes-review-menu",
                            crate::icons::Icon::ActionMenuDown,
                            "Send to another agent",
                            theme,
                            tokens,
                        )
                        .disabled(self.review.sending)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.review.menu_open = !this.review.menu_open;
                            cx.notify();
                        }))
                        .child(probe(&self.env.regions, Region::Changes("review-menu"))),
                    ),
            );
        if self.review.menu_open {
            tray = tray.child(self.render_target_menu(
                &targets,
                !drafts.is_empty(),
                theme,
                tokens,
                cx,
            ));
        }
        if let Some(notice) = &self.review.notice {
            tray = tray.child(
                div()
                    .text_color(theme.danger)
                    .text_size(px(secondary))
                    .child(SharedString::from(notice.clone())),
            );
        }
        if self.review.list_open && !ordered.is_empty() {
            tray = tray.child(self.render_review_list(&ordered, theme, tokens, cx));
        }
        Some(tray.into_any_element())
    }

    fn render_target_menu(
        &self,
        targets: &[SendTarget],
        any_drafts: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let mut menu = div()
            .flex()
            .flex_col()
            .self_end()
            .min_w(px(tokens.font_size * 22.0))
            .popover(theme, tokens);
        for (index, target) in targets.iter().enumerate() {
            let session = target.session;
            menu = menu.child(
                div()
                    .id(SharedString::from(format!("changes-review-target-{index}")))
                    .relative()
                    .row(tokens.sidebar_row, tokens)
                    .gap(px(tokens.spacing * 2.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .when(any_drafts, |s| {
                        s.on_click(
                            cx.listener(move |this, _, _, cx| this.send_review(Some(session), cx)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(target.title.clone())),
                    )
                    .child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(match (target.chat, index) {
                                (true, 0) => "chat · default",
                                (true, _) => "chat",
                                (false, 0) => "terminal · default",
                                (false, _) => "terminal",
                            }),
                    )
                    .child(probe(&self.env.regions, Region::ChangesTarget(index))),
            );
        }
        if targets.is_empty() {
            menu = menu.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .text_color(theme.muted)
                    .child("No agent works in this checkout"),
            );
        }
        menu.child(
            div()
                .id("changes-review-new-agent")
                .relative()
                .row(tokens.sidebar_row, tokens)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .when(any_drafts, |s| {
                    s.on_click(cx.listener(|this, _, _, cx| this.start_new_agent(cx)))
                })
                .child("Start a new agent with these comments")
                .child(probe(
                    &self.env.regions,
                    Region::Changes("review-new-agent"),
                )),
        )
        .into_any_element()
    }

    fn render_review_list(
        &self,
        ordered: &[&ReviewComment],
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index_of: HashMap<u64, usize> =
            ordered.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        let mut list = div()
            .id("changes-review-list")
            .flex()
            .flex_col()
            .max_h(px(tokens.font_size * 12.0))
            .overflow_y_scroll();
        let unsent: Vec<&&ReviewComment> = ordered.iter().filter(|c| c.sent_ms.is_none()).collect();
        for comment in unsent {
            list = list.child(self.render_review_row(
                comment,
                index_of[&comment.id],
                theme,
                tokens,
                cx,
            ));
        }
        let mut batches: Vec<u64> = ordered.iter().filter_map(|c| c.sent_ms).collect();
        batches.sort_unstable_by(|a, b| b.cmp(a));
        batches.dedup();
        for (n, batch) in batches.into_iter().enumerate() {
            let rows: Vec<&&ReviewComment> = ordered
                .iter()
                .filter(|c| c.sent_ms == Some(batch))
                .collect();
            let open = self.review.sent_open.contains(&batch);
            let to = rows
                .first()
                .and_then(|c| c.sent_to.clone())
                .unwrap_or_default();
            list = list.child(
                div()
                    .id(SharedString::from(format!("changes-review-batch-{n}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .h(px(tokens.sidebar_row - 4.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.review.sent_open.remove(&batch) {
                            this.review.sent_open.insert(batch);
                        }
                        cx.notify();
                    }))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(crate::icons::glyph(if open {
                        crate::icons::Icon::ActionExpand
                    } else {
                        crate::icons::Icon::ActionDisclose
                    }))
                    .child(SharedString::from(format!(
                        "Sent · {} · {} comment{} → {to}",
                        clock(batch),
                        rows.len(),
                        if rows.len() == 1 { "" } else { "s" },
                    )))
                    .child(probe(&self.env.regions, Region::ChangesBatch(n))),
            );
            if open {
                for comment in rows {
                    list = list.child(self.render_review_row(
                        comment,
                        index_of[&comment.id],
                        theme,
                        tokens,
                        cx,
                    ));
                }
            }
        }
        list.into_any_element()
    }

    fn render_review_row(
        &self,
        comment: &ReviewComment,
        index: usize,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = comment.id;
        let hover = theme.surface_hover;
        let focused = self.review.focused == Some(id);
        let state = state_label(comment);
        div()
            .id(SharedString::from(format!("changes-review-row-{index}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row - 4.0))
            .px(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(focused, |s| s.bg(theme.accent_soft))
            .when(!focused, move |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.jump_to(id, cx)))
            .child(
                div()
                    .flex_none()
                    .font_family(Look::of(cx).mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(comment.anchor())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if comment.resolved {
                        theme.faint
                    } else {
                        theme.fg
                    })
                    .child(SharedString::from(comment.headline().to_owned())),
            )
            .when_some(state, |s, (label, color)| {
                s.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(color(theme))
                        .child(label),
                )
            })
            .child(probe(&self.env.regions, Region::ChangesTray(index)))
            .into_any_element()
    }

    /// What is drawn for the comments of `file`: under which line, and in which order.
    pub(super) fn anchors(&self, file: &str) -> Anchors {
        let ordered = self.ordered();
        let mut anchors = Anchors {
            last: HashMap::new(),
            index: HashMap::new(),
        };
        for (i, comment) in ordered.iter().enumerate() {
            anchors.index.insert(comment.id, i);
            if comment.file == file
                && let Some((_, last)) = self.review.placed.get(&comment.id)
            {
                anchors.last.entry(*last).or_default().push(comment.id);
            }
        }
        anchors
    }

    /// The comment cards and the comment box of the shown file's outdated comments, after its
    /// diff.
    pub(super) fn render_outdated(
        &self,
        file: &str,
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let anchors = self.anchors(file);
        let ordered = self.ordered();
        ordered
            .iter()
            .filter(|c| c.file == file && !self.review.placed.contains_key(&c.id))
            .map(|c| self.render_comment_or_box(c.id, &anchors, look, window, cx))
            .collect()
    }

    /// Draws diff line `at` of `file` into `block`, with what hangs below it: the comments that
    /// end there and the comment box.
    #[allow(clippy::too_many_arguments)] // the diff row and everything it is drawn from
    pub(super) fn review_line<E: ParentElement>(
        &self,
        mut block: E,
        anchors: &Anchors,
        file: &str,
        at: usize,
        line: &DiffLine,
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> E {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let span = self
            .review
            .selection
            .as_ref()
            .filter(|s| s.file == file)
            .map(|s| s.span());
        let selected = span.is_some_and(|(a, b)| a <= at && at <= b);
        let tail = selected && span.is_some_and(|(_, b)| b == at) && self.review.draft.is_none();
        let numbered = !matches!(line.kind, LineKind::Hunk | LineKind::Note);
        let mut row = div().relative().min_w_full().child(diff_row(line, look));
        if selected {
            row = row.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(theme.accent.opacity(0.2)),
            );
        }
        if numbered {
            row = row.child(
                div()
                    .id(SharedString::from(format!("changes-line-{at}")))
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px(tokens.font_size * 6.4))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.select_line(at, event.modifiers.shift, cx);
                            this.review.dragging = true;
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left) && this.review.dragging {
                            this.drag_to(at, cx);
                        } else {
                            this.review.dragging = false;
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, _| this.review.dragging = false),
                    )
                    .child(probe(&self.env.regions, Region::ChangesLine(at))),
            );
        }
        if tail {
            row = row.child(
                div()
                    .id("changes-comment-add")
                    .absolute()
                    .right(px(tokens.spacing * 2.0))
                    .top_0()
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.accent)
                    .text_color(theme.on_accent())
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_comment_box(window, cx);
                    }))
                    .child("+ Comment")
                    .child(probe(&self.env.regions, Region::Changes("comment-add"))),
            );
        }
        block = block.child(row);
        let draft_here = self
            .review
            .draft
            .as_ref()
            .is_some_and(|d| d.editing.is_none() && d.file == file && d.to == at);
        for id in anchors.last.get(&at).into_iter().flatten() {
            block = block.child(
                div()
                    .min_w_full()
                    .child(self.render_comment_or_box(*id, anchors, look, window, cx)),
            );
        }
        if draft_here {
            block = block.child(
                div()
                    .min_w_full()
                    .child(self.render_comment_box(None, look, window, cx)),
            );
        }
        block
    }

    fn render_comment_or_box(
        &self,
        id: u64,
        anchors: &Anchors,
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self
            .review
            .draft
            .as_ref()
            .is_some_and(|d| d.editing == Some(id))
        {
            return self.render_comment_box(Some(id), look, window, cx);
        }
        match self.comment(id) {
            Some(comment) => {
                let index = anchors.index.get(&id).copied().unwrap_or(0);
                self.render_comment(comment, index, look, cx)
            }
            None => div().into_any_element(),
        }
    }

    fn render_comment_box(
        &self,
        editing: Option<u64>,
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let Some(draft) = &self.review.draft else {
            return div().into_any_element();
        };
        let focused = self.review.comment_focus.is_focused(window);
        let vim = VimConfig::of(&self.env.settings).enabled;
        let title = match editing.and_then(|id| self.comment(id)) {
            Some(comment) => format!("Edit the comment on {}", comment.anchor()),
            None => {
                let (from, to) = (draft.from, draft.to);
                let lines = self
                    .selected_diff()
                    .map(|d| selection_label(&draft.file, d, from, to))
                    .unwrap_or_else(|| draft.file.clone());
                if draft.outdated {
                    format!(
                        "Comment on {} — those lines changed; it will be saved as outdated",
                        draft.file
                    )
                } else {
                    format!("Comment on {lines}")
                }
            }
        };
        let empty = draft.input.text().trim().is_empty();
        div()
            .font_family(SharedString::from(UI_FONT))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .mx(px(tokens.spacing * 3.0))
            .my(px(tokens.spacing))
            .p(px(tokens.spacing * 2.0))
            .card(theme, tokens)
            .whitespace_normal()
            .text_size(px(tokens.font_size))
            .child(div().medium().child(SharedString::from(title)))
            .child(
                field_box(
                    "changes-comment-box",
                    &draft.input,
                    vim.then_some(&draft.vim),
                    "Leave a comment…",
                    focused,
                    true,
                    Some(self.comment_field(cx)),
                    theme,
                    tokens,
                )
                .relative()
                .track_focus(&self.review.comment_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_comment_key(event, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        // The workbench takes focus on this same mouse down; ask again after it.
                        let handle = this.review.comment_focus.clone();
                        cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                        cx.stop_propagation();
                    }),
                )
                .child(probe(&self.env.regions, Region::Changes("comment-box"))),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        ui::Button::new("changes-comment-cancel", "Cancel", theme, tokens)
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel_comment(cx);
                                window.focus(&this.review.focus, cx);
                            }))
                            .child(probe(&self.env.regions, Region::Changes("comment-cancel"))),
                    )
                    .child(
                        ui::Button::new(
                            "changes-comment-save",
                            if editing.is_some() { "Save" } else { "Comment" },
                            theme,
                            tokens,
                        )
                        .primary()
                        .shortcut(self.send_key().combo())
                        .disabled(empty)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.add_comment(cx) {
                                window.focus(&this.review.focus, cx);
                            }
                        }))
                        .child(probe(&self.env.regions, Region::Changes("comment-save"))),
                    ),
            )
            .into_any_element()
    }

    fn render_comment(
        &self,
        comment: &ReviewComment,
        index: usize,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let id = comment.id;
        let focused = self.review.focused == Some(id);
        let reveal = self.review.reveal.clone();
        let scroll = self.review.scroll.clone();
        let tone = state_label(comment).map_or(theme.accent, |(_, color)| color(theme));
        let state = state_label(comment);
        let action = |name: &'static str, label: &str| {
            ui::Button::new(
                SharedString::from(format!("changes-comment-{index}-{name}")),
                SharedString::from(label.to_owned()),
                theme,
                tokens,
            )
            .compact()
            .ghost()
            .child(probe(
                &self.env.regions,
                Region::ChangesComment(index, name),
            ))
        };
        let mut card =
            div()
                .relative()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .mx(px(tokens.spacing * 3.0))
                .my(px(tokens.spacing))
                .p(px(tokens.spacing * 2.0))
                .card(theme, tokens)
                .overflow_hidden()
                .whitespace_normal()
                .font_family(SharedString::from(UI_FONT))
                .text_size(px(tokens.font_size))
                .when(focused, |s| s.bg(theme.accent_soft))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(px(3.0))
                        .bg(tone),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .text_size(px(tokens.secondary_font_size))
                        .child(div().medium().child("you"))
                        .child(
                            div()
                                .text_color(theme.muted)
                                .child(SharedString::from(age_label(comment.created_ms))),
                        )
                        .child(
                            div()
                                .font_family(look.mono.clone())
                                .text_color(theme.muted)
                                .child(SharedString::from(comment.anchor())),
                        )
                        .when_some(state, |s, (label, color)| {
                            s.child(ui::state_label(
                                ui::state_glyph(color(theme), theme),
                                label,
                                color(theme),
                                false,
                                theme,
                                tokens,
                            ))
                        })
                        .child(div().flex_1())
                        .child(action("edit", "Edit").on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.edit_comment(id, window, cx);
                            },
                        )))
                        .child(action("delete", "Delete").on_click(
                            cx.listener(move |this, _, _, cx| this.delete_comment(id, cx)),
                        ))
                        .child(
                            action(
                                "send",
                                if comment.sent_ms.is_some() {
                                    "Send again"
                                } else {
                                    "Send now"
                                },
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.send_comment(id, cx))),
                        )
                        .child(
                            action(
                                "resolve",
                                if comment.resolved {
                                    "Unresolve"
                                } else {
                                    "Resolve"
                                },
                            )
                            .on_click(cx.listener({
                                let resolved = comment.resolved;
                                move |this, _, _, cx| this.set_resolved(id, !resolved, cx)
                            })),
                        ),
                )
                .child(
                    div()
                        .text_color(if comment.resolved {
                            theme.muted
                        } else {
                            theme.fg
                        })
                        .child(SharedString::from(comment.text.clone())),
                );
        if comment.outdated {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.font_size - 0.5))
                    .text_color(theme.muted)
                    .child(
                        div()
                            .font_family(SharedString::from(UI_FONT))
                            .text_size(px(tokens.secondary_font_size))
                            .child("The lines changed; this is what the comment was about:"),
                    )
                    .children(diff_text(&comment.lines).lines().map(|line| {
                        div()
                            .whitespace_nowrap()
                            .child(SharedString::from(line.to_owned()))
                    })),
            );
        }
        card.child(
            gpui::canvas(
                {
                    let regions = self.env.regions.clone();
                    move |bounds, window, _| {
                        regions.record_prepaint(Region::ChangesComment(index, "card"), bounds);
                        if reveal.get() == Some(id) {
                            reveal.set(None);
                            let view = scroll.bounds();
                            if bounds.top() < view.top() || bounds.bottom() > view.bottom() {
                                let mut offset = scroll.offset();
                                offset.y += view.top() + px(8.0) - bounds.top();
                                scroll.set_offset(offset);
                                window.refresh();
                            }
                        }
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .into_any_element()
    }
}

/// The badge of a comment's state with its colour.
type Tone = fn(&Theme) -> gpui::Hsla;

fn state_label(comment: &ReviewComment) -> Option<(&'static str, Tone)> {
    if comment.resolved {
        Some(("Resolved", |t| t.success))
    } else if comment.outdated {
        Some(("Outdated", |t| t.warning))
    } else if comment.sent_ms.is_some() {
        Some(("Sent", |t| t.accent))
    } else {
        None
    }
}

/// `a.txt:12-14` for the diff lines `from..=to`.
fn selection_label(
    file: &str,
    diff: &hutan_chat_view::diff::FileDiff,
    from: usize,
    to: usize,
) -> String {
    let numbers: Vec<u32> = diff.lines[from.min(diff.lines.len())..(to + 1).min(diff.lines.len())]
        .iter()
        .filter(|l| Mark::of_kind(l.kind).is_some())
        .filter_map(|l| l.new.or(l.old))
        .collect();
    match (numbers.iter().min(), numbers.iter().max()) {
        (Some(a), Some(b)) if a == b => format!("{file}:{a}"),
        (Some(a), Some(b)) => format!("{file}:{a}-{b}"),
        _ => file.to_owned(),
    }
}

/// Applies what a patch says to a comment: `found` lines are open again, lines that are gone
/// make it outdated — and a sent comment resolved, unless the user reopened it. Only a patch
/// with the uncommitted changes (`full`) says that for sure.
fn settle(comment: &mut ReviewComment, found: bool, full: bool) -> bool {
    let mut changed = false;
    if found {
        if comment.outdated {
            comment.outdated = false;
        }
    } else if full {
        comment.outdated = true;
        if comment.sent_ms.is_some() && !comment.resolved && !comment.keep_open {
            comment.resolved = true;
            changed = true;
        }
    }
    changed
}
