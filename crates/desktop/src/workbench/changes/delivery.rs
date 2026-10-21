//! Delivery in the Changes view (DL-1, protocol 0.25, capability `task.review`): the review of
//! the task whose work the checkout holds — **Accept**, **Request changes…** with a note — and
//! the truth about what a merge takes: the commits on the branch (what the merge step of
//! **Finish** integrates) apart from the uncommitted changes of the worktree (which Finish
//! commits first), with **Commit all…** to commit them without merging, and the cleanup offer
//! after a merge that left uncommitted changes behind (removing the worktree would discard
//! them: it defaults to *Keep*).
//!
//! The daemon owns the delivery state: this module sends `task.review` and `worktree.commit`
//! and draws what `task.updated` and `worktree.diff` say.

use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, px,
};
use hutan_core::{DeliveryState, ReviewDecision, Task as DaemonTask, TaskId, TaskState};
use hutan_protocol::worktree::{CommitSummary, DiffFile, WorktreeCommit, WorktreeMerged};

use super::{ChangesEvent, ChangesView, Listing};
use crate::sessions::tasks::delivery_pill;
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::vim::{Outcome, Vim};
use crate::workbench::chat::{Look, TextInput};
use crate::workbench::panels::{edit_text, field_box, tone_color};
use crate::workbench::render::probe;
use crate::workbench::text_field::{FieldMut, FieldRef};
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, VimConfig};

/// How many commits and files a scope section lists before it says "and N more".
const LISTED: usize = 8;

/// What the offer to remove the worktree after a merge defaults to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupDefault {
    /// Nothing is left in the worktree: removing it (and its merged branch) is the default.
    Remove,
    /// Uncommitted changes are left: removing the worktree would discard them, so *Keep* is the
    /// default and removal is a deliberate second choice.
    Keep,
}

impl CleanupDefault {
    /// The default for the worktree a merge `result` came from.
    pub fn of(result: &WorktreeMerged) -> Self {
        if result.source_dirty {
            Self::Keep
        } else {
            Self::Remove
        }
    }
}

/// Which text form of the delivery is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryForm {
    /// "Request changes…": the note for the agent.
    RequestChanges,
    /// "Commit all…": the commit message.
    Commit,
}

#[derive(Debug)]
struct TextForm {
    kind: DeliveryForm,
    input: TextInput,
    /// Vim's modal state over the text (`editor.vim_mode`).
    vim: Vim,
    running: bool,
    error: Option<String>,
}

/// The delivery state of one Changes view.
#[derive(Debug)]
pub(super) struct Delivery {
    /// The task the view was opened for (a task row's "Changes"); else the worktree's branch
    /// says which task it is.
    hint: Option<TaskId>,
    form: Option<TextForm>,
    /// A `task.review` is on its way.
    reviewing: bool,
    focus: FocusHandle,
}

impl Delivery {
    pub(super) fn new(cx: &mut Context<ChangesView>) -> Self {
        Self {
            hint: None,
            form: None,
            reviewing: false,
            focus: cx.focus_handle(),
        }
    }

    /// Closes the open form.
    pub(super) fn close(&mut self) {
        self.form = None;
    }
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

impl ChangesView {
    // ---- read access (also for tests) -----------------------------------------------------

    /// The task whose work this checkout holds: the one the view was opened for, else the one
    /// on the worktree's branch (waiting for its delivery, or the latest).
    pub fn task(&self, cx: &App) -> Option<DaemonTask> {
        let book = self.env.sessions.read(cx).tasks();
        if let Some(task) = self.delivery.hint.and_then(|id| book.get(id)) {
            return Some(task.clone());
        }
        let info = self.info.as_ref().filter(|i| !i.main)?;
        book.for_branch(info.branch.as_deref()?).cloned()
    }

    /// Where the checkout's task delivery is; `None` while the view has no task.
    pub fn delivery_state(&self, cx: &App) -> Option<DeliveryState> {
        self.task(cx).map(|task| task.delivery)
    }

    /// Whether the task can be reviewed now: the daemon knows `task.review`, the task is `done`
    /// and its work is neither merged nor discarded.
    pub fn can_review(&self, cx: &App) -> bool {
        self.env
            .sessions
            .read(cx)
            .connection()
            .supports_task_review()
            && self
                .task(cx)
                .is_some_and(|t| t.state == TaskState::Done && t.delivery.is_open())
    }

    /// The open delivery form, if any.
    pub fn delivery_form(&self) -> Option<DeliveryForm> {
        self.delivery.form.as_ref().map(|f| f.kind)
    }

    /// The refusal of the open delivery form's request.
    pub fn delivery_form_error(&self) -> Option<&str> {
        self.delivery.form.as_ref().and_then(|f| f.error.as_deref())
    }

    /// The commits a merge would integrate, newest first (`worktree.diff`).
    pub fn commits(&self) -> &[CommitSummary] {
        match &self.listing {
            Listing::Ready(diff) => &diff.commits,
            _ => &[],
        }
    }

    /// The changed files a merge takes (they changed in a commit on the branch).
    pub fn committed_files(&self) -> Vec<&DiffFile> {
        self.files().iter().filter(|f| f.committed).collect()
    }

    /// The changed files a merge would not take (staged, unstaged or untracked changes).
    pub fn uncommitted_files(&self) -> Vec<&DiffFile> {
        self.files().iter().filter(|f| f.uncommitted).collect()
    }

    /// The worktree has changes that are in no commit: listed, or (with the toggle off) known
    /// from `worktree.list`.
    pub fn has_uncommitted(&self) -> bool {
        self.files().iter().any(|f| f.uncommitted) || self.info.as_ref().is_some_and(|i| i.dirty)
    }

    /// What the cleanup offer after the merge in this view defaults to.
    pub fn cleanup_default(&self) -> Option<CleanupDefault> {
        self.merged().map(CleanupDefault::of)
    }

    /// Whether the scope sections apply: a worktree whose daemon says what is committed.
    fn shows_scope(&self, cx: &App) -> bool {
        self.can_merge()
            && self.merged.is_none()
            && self
                .env
                .sessions
                .read(cx)
                .connection()
                .supports_task_review()
            && matches!(self.listing, Listing::Ready(_))
    }

    // ---- actions --------------------------------------------------------------------------

    /// Names the task this view reviews (a task row's "Changes").
    pub fn set_task(&mut self, task: Option<TaskId>, cx: &mut Context<Self>) {
        self.delivery.hint = task;
        cx.notify();
    }

    /// "Accept": a person accepted the work (`task.review`).
    pub fn accept(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.task(cx) else {
            return;
        };
        if !self.can_review(cx)
            || self.delivery.reviewing
            || task.delivery == DeliveryState::Accepted
        {
            return;
        }
        self.delivery.reviewing = true;
        let reviewing = self.env.sessions.update(cx, |store, cx| {
            store.review_task(task.id, ReviewDecision::Accept, None, cx)
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = reviewing.await;
            this.update(cx, |this, cx| {
                this.delivery.reviewing = false;
                cx.emit(ChangesEvent::Notice(match result {
                    Ok(_) => format!("Accepted “{}”", task.title),
                    Err(error) => format!("Cannot accept the task: {error}"),
                }));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_form(&mut self, kind: DeliveryForm, window: &mut Window, cx: &mut Context<Self>) {
        if self.merged.is_some() {
            return;
        }
        self.finish = None;
        self.delivery.form = Some(TextForm {
            kind,
            input: TextInput::new(),
            vim: Vim::new(),
            running: false,
            error: None,
        });
        window.focus(&self.delivery.focus, cx);
        cx.notify();
    }

    /// "Request changes…": opens the note form.
    pub fn open_request_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_review(cx) {
            self.open_form(DeliveryForm::RequestChanges, window, cx);
        }
    }

    /// "Commit all…": opens the commit message form.
    pub fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_merge() && self.has_uncommitted() {
            self.open_form(DeliveryForm::Commit, window, cx);
        }
    }

    pub fn close_delivery_form(&mut self, cx: &mut Context<Self>) {
        self.delivery.form = None;
        cx.notify();
    }

    /// Sets the text of the open delivery form (the note, or the commit message).
    pub fn set_delivery_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.delivery.form {
            form.input.set(text);
            cx.notify();
        }
    }

    /// Sends what the open form stands for: `task.review` (request changes) or `worktree.commit`.
    pub fn confirm_delivery_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.delivery.form else {
            return;
        };
        if form.running {
            return;
        }
        match form.kind {
            DeliveryForm::RequestChanges => self.send_request_changes(cx),
            DeliveryForm::Commit => self.send_commit(cx),
        }
    }

    fn send_request_changes(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.task(cx) else {
            return;
        };
        let Some(form) = &mut self.delivery.form else {
            return;
        };
        form.running = true;
        form.error = None;
        let note = form.input.text().trim().to_owned();
        let note = (!note.is_empty()).then_some(note);
        let sent = note.is_some();
        let reviewing = self.env.sessions.update(cx, |store, cx| {
            store.review_task(task.id, ReviewDecision::RequestChanges, note, cx)
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = reviewing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(reviewed) => {
                        this.delivery.form = None;
                        cx.emit(ChangesEvent::Notice(match (reviewed.session, sent) {
                            (Some(_), _) => format!(
                                "Asked for changes on “{}”; the note went to its chat",
                                task.title
                            ),
                            (None, true) => format!(
                                "Asked for changes on “{}”; the note stays on the task",
                                task.title
                            ),
                            (None, false) => format!("Asked for changes on “{}”", task.title),
                        }));
                    }
                    Err(error) => {
                        if let Some(form) = &mut this.delivery.form {
                            form.running = false;
                            form.error = Some(error);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_commit(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.delivery.form else {
            return;
        };
        let message = form.input.text().trim().to_owned();
        if message.is_empty() {
            form.error = Some("A commit needs a message.".to_owned());
            cx.notify();
            return;
        }
        form.running = true;
        form.error = None;
        let request = WorktreeCommit {
            path: self.path.clone(),
            message,
        };
        let committing = self
            .env
            .sessions
            .update(cx, |store, cx| store.commit_worktree(request, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = committing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(done) => {
                        this.delivery.form = None;
                        cx.emit(ChangesEvent::Notice(format!(
                            "Committed {} on {} ({})",
                            plural(done.files as usize, "file", "files"),
                            done.branch,
                            short(&done.commit)
                        )));
                        this.reload(cx);
                    }
                    Err(error) => {
                        if let Some(form) = &mut this.delivery.form {
                            form.running = false;
                            form.error = Some(error);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_delivery_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        match event.keystroke.key.as_str() {
            "escape" => {
                if vim.enabled
                    && let Some(form) = &mut self.delivery.form
                    && vim_ui::apply(
                        &mut form.vim,
                        &mut form.input,
                        event,
                        vim.options(false, true),
                        cx,
                    ) == Outcome::Handled
                {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                self.close_delivery_form(cx);
            }
            "enter" if event.keystroke.modifiers.platform => self.confirm_delivery_form(cx),
            _ => {
                let Some(form) = &mut self.delivery.form else {
                    return;
                };
                if !edit_text(&mut form.input, &mut form.vim, vim, event, true, cx) {
                    return;
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// How the form's element reaches its text: the platform's text input and the mouse.
    fn delivery_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.delivery.focus.clone(),
            |this, _, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(form) = &mut this.delivery.form {
                    f(&mut FieldMut {
                        input: &mut form.input,
                        vim: vim_on.then_some(&mut form.vim),
                        multiline: true,
                    });
                }
            },
        )
    }

    // ---- drawing --------------------------------------------------------------------------

    /// The delivery pill and the review buttons of the header.
    pub(super) fn render_review_controls(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(task) = self.task(cx) else {
            return Vec::new();
        };
        let mut controls = Vec::new();
        if let Some((label, tone)) = delivery_pill(task.delivery) {
            controls.push(
                ui::state_label(
                    ui::state_glyph(tone_color(theme, tone), theme),
                    label,
                    tone_color(theme, tone),
                    false,
                    theme,
                    tokens,
                )
                .relative()
                .child(probe(&self.env.regions, Region::Changes("delivery")))
                .into_any_element(),
            );
        }
        if self.can_review(cx) {
            let accepted = task.delivery == DeliveryState::Accepted;
            let accept_enabled = !accepted && !self.delivery.reviewing;
            controls.push(
                ui::button(
                    "changes-accept",
                    if accepted { "Accepted" } else { "Accept" },
                    ui::ButtonKind::Secondary,
                    accept_enabled,
                    theme,
                    tokens,
                )
                .relative()
                .when(accept_enabled, |s| {
                    s.on_click(cx.listener(|this, _, _, cx| this.accept(cx)))
                })
                .child(probe(&self.env.regions, Region::Changes("accept")))
                .into_any_element(),
            );
            controls.push(
                ui::button(
                    "changes-request-changes",
                    "Request changes…",
                    ui::ButtonKind::Secondary,
                    true,
                    theme,
                    tokens,
                )
                .relative()
                .on_click(cx.listener(|this, _, window, cx| this.open_request_changes(window, cx)))
                .child(probe(&self.env.regions, Region::Changes("request-changes")))
                .into_any_element(),
            );
        }
        controls
    }

    /// The delivery form and the scope card, between the header and the rest.
    pub(super) fn render_delivery(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut parts = Vec::new();
        if self.delivery.form.is_some() {
            parts.push(self.render_delivery_form(theme, tokens, window, cx));
        }
        if self.shows_scope(cx) {
            parts.push(self.render_scope(theme, tokens, cx));
        }
        parts
    }

    fn render_delivery_form(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(form) = &self.delivery.form else {
            return div().into_any_element();
        };
        let (title, hint, placeholder, send, key_field, key_send, key_cancel) = match form.kind {
            DeliveryForm::RequestChanges => (
                format!(
                    "Request changes on “{}”",
                    self.task(cx).map(|t| t.title).unwrap_or_default()
                ),
                "The note goes to the task's chat as a message and the task runs again. A task \
                 that ran in a terminal cannot be written to: the note stays on the task for \
                 whoever owns it.",
                "What should change? (optional)",
                if form.running {
                    "Sending…"
                } else {
                    "Request changes"
                },
                "request-note",
                "request-send",
                "request-cancel",
            ),
            DeliveryForm::Commit => (
                format!(
                    "Commit everything in {}",
                    self.info
                        .as_ref()
                        .and_then(|i| i.branch.clone())
                        .unwrap_or_else(|| "the worktree".to_owned())
                ),
                "Stages and commits every change in the worktree, untracked files included, as one \
                 commit on its branch, so the merge takes it. Git hooks do not run.",
                "Commit message",
                if form.running {
                    "Committing…"
                } else {
                    "Commit"
                },
                "commit-message",
                "commit-confirm",
                "commit-cancel",
            ),
        };
        let focused = self.delivery.focus.is_focused(window);
        let vim = VimConfig::of(&self.env.settings).enabled;
        let mut body = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .m(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(div().medium().child(SharedString::from(title)))
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(hint),
            )
            .child(
                field_box(
                    SharedString::from(format!("changes-{key_field}")),
                    &form.input,
                    vim.then_some(&form.vim),
                    placeholder,
                    focused,
                    true,
                    Some(self.delivery_field(cx)),
                    theme,
                    tokens,
                )
                .relative()
                .track_focus(&self.delivery.focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    this.on_delivery_key(event, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        // The workbench takes focus on this same mouse down; ask again after it.
                        let handle = this.delivery.focus.clone();
                        cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                    }),
                )
                .child(probe(&self.env.regions, Region::Changes(key_field))),
            );
        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        body.child(
            div()
                .flex()
                .justify_end()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    ui::button(
                        SharedString::from(format!("changes-{key_cancel}")),
                        "Cancel",
                        ui::ButtonKind::Ghost,
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.close_delivery_form(cx)))
                    .child(probe(&self.env.regions, Region::Changes(key_cancel))),
                )
                .child(
                    ui::button(
                        SharedString::from(format!("changes-{key_send}")),
                        send,
                        ui::ButtonKind::Primary,
                        !form.running,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_delivery_form(cx)))
                    .child(probe(&self.env.regions, Region::Changes(key_send))),
                ),
        )
        .into_any_element()
    }

    /// A line of a scope section: monospace, one hash or file.
    fn scope_lines(
        &self,
        lines: Vec<String>,
        more: usize,
        theme: &Theme,
        tokens: &Tokens,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .font_family(Look::of(cx).mono.clone())
            .text_size(px(tokens.secondary_font_size))
            .children(
                lines
                    .into_iter()
                    .map(|line| div().truncate().child(SharedString::from(line))),
            )
            .when(more > 0, |s| {
                s.child(
                    div()
                        .text_color(theme.muted)
                        .child(SharedString::from(format!("… and {more} more"))),
                )
            })
            .into_any_element()
    }

    /// The merge scope: what is committed on the branch (a merge takes it) and what is only in
    /// the worktree (a merge does not).
    fn render_scope(&self, theme: &Theme, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let branch = self
            .info
            .as_ref()
            .and_then(|i| i.branch.clone())
            .unwrap_or_default();
        let target = self.base().unwrap_or("main").to_owned();
        let commits = self.commits();
        let committed = self.committed_files();
        let uncommitted = self.uncommitted_files();
        let more = |total: usize| total.saturating_sub(LISTED);

        let committed_head = if commits.is_empty() && committed.is_empty() {
            format!("Committed on {branch}: nothing yet.")
        } else {
            format!(
                "Committed on {branch}: {}, {}. Finish merges these into {target}.",
                plural(commits.len(), "commit", "commits"),
                plural(committed.len(), "file", "files"),
            )
        };
        let mut card = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .mx(px(tokens.spacing * 2.0))
            .mt(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(
                        div()
                            .medium()
                            .text_color(theme.success)
                            .child(SharedString::from(committed_head))
                            .relative()
                            .child(probe(&self.env.regions, Region::Changes("scope-committed"))),
                    )
                    .child(
                        self.scope_lines(
                            commits
                                .iter()
                                .take(LISTED)
                                .map(|c| format!("{} {}", short(&c.id), c.subject))
                                .collect(),
                            more(commits.len()) + usize::from(self.commits_truncated()),
                            theme,
                            tokens,
                            cx,
                        ),
                    ),
            );

        let dirty = self.has_uncommitted();
        if dirty {
            let head = if uncommitted.is_empty() {
                "Uncommitted in the worktree: changes that are not listed here. Finish commits \
                 them first."
                    .to_owned()
            } else {
                format!(
                    "Uncommitted in the worktree: {}. Finish commits them first.",
                    plural(uncommitted.len(), "file", "files")
                )
            };
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(
                        // Commit all… wraps under the sentence in a narrow pane instead of
                        // being pushed out of it.
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(px(tokens.spacing * 2.0))
                            .gap_y(px(tokens.spacing))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(tokens.font_size * 14.0))
                                    .medium()
                                    .text_color(theme.warning)
                                    .child(SharedString::from(head))
                                    .relative()
                                    .child(probe(
                                        &self.env.regions,
                                        Region::Changes("scope-uncommitted"),
                                    )),
                            )
                            .child(
                                ui::button(
                                    "changes-commit-all",
                                    "Commit all…",
                                    ui::ButtonKind::Secondary,
                                    true,
                                    theme,
                                    tokens,
                                )
                                .relative()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_commit(window, cx);
                                }))
                                .child(probe(&self.env.regions, Region::Changes("commit-all"))),
                            ),
                    )
                    .child(
                        self.scope_lines(
                            uncommitted
                                .iter()
                                .take(LISTED)
                                .map(|f| f.path.clone())
                                .collect(),
                            more(uncommitted.len()),
                            theme,
                            tokens,
                            cx,
                        ),
                    ),
            );
        }
        card.into_any_element()
    }

    fn commits_truncated(&self) -> bool {
        matches!(&self.listing, Listing::Ready(diff) if diff.commits_truncated)
    }

    /// The merge preview inside the Finish form of a worktree: exactly the commits the merge
    /// integrates, and the uncommitted changes the commit step adds to them.
    pub(super) fn render_merge_takes(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let commits = self.commits();
        let mut column = div().flex().flex_col().gap(px(tokens.spacing));
        if !matches!(self.listing, Listing::Ready(_))
            || !self
                .env
                .sessions
                .read(cx)
                .connection()
                .supports_task_review()
        {
            return column.into_any_element();
        }
        column = column
            .child(
                div()
                    .medium()
                    .child(SharedString::from(if commits.is_empty() {
                        "The merge takes no commit yet.".to_owned()
                    } else {
                        format!(
                            "The merge takes {}:",
                            plural(commits.len(), "commit", "commits")
                        )
                    }))
                    .relative()
                    .child(probe(&self.env.regions, Region::Changes("finish-commits"))),
            )
            .child(
                self.scope_lines(
                    commits
                        .iter()
                        .take(LISTED)
                        .map(|c| format!("{} {}", short(&c.id), c.subject))
                        .collect(),
                    commits.len().saturating_sub(LISTED) + usize::from(self.commits_truncated()),
                    theme,
                    tokens,
                    cx,
                ),
            );
        if self.has_uncommitted() {
            let uncommitted = self.uncommitted_files().len();
            column = column.child(
                div()
                    .text_color(theme.warning)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(if uncommitted == 0 {
                        "First, the uncommitted changes of the worktree are committed with the \
                         message below, so the merge takes them too."
                            .to_owned()
                    } else {
                        format!(
                            "First, {} with uncommitted changes {} committed with the message \
                             below, so the merge takes them too.",
                            plural(uncommitted, "file", "files"),
                            if uncommitted == 1 { "is" } else { "are" }
                        )
                    })),
            );
        }
        column.into_any_element()
    }
}
