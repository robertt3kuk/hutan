//! **Finish** in the Changes view (PR-FINISH, protocol 0.35, capability `finish`, ADR 0020): one
//! action for both placements. A main checkout is committed (everything listed, untracked files
//! included) and its branch pushed to its upstream; a worktree is committed, merged into the
//! main checkout's branch with one of the three merge modes, and that branch pushed. The message
//! starts as what the commit changes, in git's own words (`Update app.py`, `Add todo.md`,
//! `Update 4 files in src/`; FX-E2E2-A), with one body line per task whose turns changed files
//! in the checkout (`Agent: OMP — Fix the greeting`, newest first; never a chat that changed
//! nothing), and stays editable; *Push* starts as `[finish] push` of `daemon.toml`.
//!
//! The daemon does the work (`worktree.finish`) and answers step by step; the view shows one
//! line per step with git's own reason when one failed (no upstream, a push that would not be a
//! fast-forward, credentials), offers the worktree cleanup after a merge and reads the checkout
//! again. A refusal before anything changed stays in the form.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use hutan_core::{SessionId, TaskRun};
use hutan_protocol::worktree::{
    DiffFile, DiffStatus, FinishCommit, FinishMerge, FinishPush, MergeStrategy, StepReason,
    StepStatus, WorktreeFinish, WorktreeFinished, WorktreeMerged,
};

use super::{ChangesEvent, ChangesView, Merged};
use crate::sessions::presentation::harness_label;
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::vim::{Outcome, Vim};
use crate::workbench::chat::{Look, TextInput};
use crate::workbench::panels::{chip, edit_text, field_box};
use crate::workbench::render::probe;
use crate::workbench::text_field::{FieldMut, FieldRef};
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, VimConfig};

/// The `daemon.toml` setting that says whether a Finish pushes.
const PUSH_SETTING: &str = "finish.push";

/// The open Finish form.
#[derive(Debug)]
pub(super) struct FinishForm {
    /// How a worktree's branch is merged (ignored for a main checkout).
    strategy: MergeStrategy,
    message: TextInput,
    /// What the message was prefilled with: until the user edits it, it follows the listing.
    prefilled: String,
    /// Vim's modal state over the message (`editor.vim_mode`).
    vim: Vim,
    /// Push after committing (and merging).
    push: bool,
    /// The user chose `push` here; until then `[finish] push` decides.
    push_chosen: bool,
    running: bool,
    /// The daemon refused before changing anything.
    error: Option<String>,
}

fn short(hash: Option<&str>) -> String {
    hash.unwrap_or_default().chars().take(7).collect()
}

fn files(n: u32) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// The subject of a Finish commit in git's own words, from the files it takes: `Update app.py`,
/// `Update app.py and store.rs`, `Update 4 files in src/` (the directory they share, if any);
/// `Add …` when every file is new (untracked), `Delete …` when every file goes. Empty without
/// files.
pub fn commit_subject(files: &[&DiffFile]) -> String {
    let verb = if files.iter().all(|f| f.status == DiffStatus::Added) {
        "Add"
    } else if files.iter().all(|f| f.status == DiffStatus::Deleted) {
        "Delete"
    } else {
        "Update"
    };
    let name = |file: &DiffFile| -> String {
        file.path
            .rsplit('/')
            .next()
            .unwrap_or(&file.path)
            .to_owned()
    };
    match files {
        [] => String::new(),
        [one] => format!("{verb} {}", name(one)),
        [a, b] => {
            let (first, second) = (name(a), name(b));
            // Two files of the same name say where they are.
            if first == second {
                format!("{verb} {} and {}", a.path, b.path)
            } else {
                format!("{verb} {first} and {second}")
            }
        }
        many => match common_dir(many.iter().map(|f| f.path.as_str())) {
            Some(dir) => format!("{verb} {} files in {dir}/", many.len()),
            None => format!("{verb} {} files", many.len()),
        },
    }
}

/// The directory every one of `paths` (`/`-separated, relative) is in; `None` at the top level.
fn common_dir<'a>(paths: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut common: Option<Vec<&str>> = None;
    for path in paths {
        let mut dirs: Vec<&str> = path.split('/').collect();
        dirs.pop();
        common = Some(match common {
            None => dirs,
            Some(so_far) => so_far
                .into_iter()
                .zip(dirs)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    common
        .filter(|dirs| !dirs.is_empty())
        .map(|dirs| dirs.join("/"))
}

/// A body line of the Finish message naming who made a change: `Agent: OMP — Fix the
/// greeting` (the harness and the chat's title); `Task: <title>` when no harness is known.
pub fn agent_line(harness: Option<&str>, title: &str) -> String {
    match harness {
        Some(harness) => format!("Agent: {harness} — {title}"),
        None => format!("Task: {title}"),
    }
}

/// How a step went, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepTone {
    Done,
    Skipped,
    Failed,
}

impl From<StepStatus> for StepTone {
    fn from(status: StepStatus) -> Self {
        match status {
            StepStatus::Ok => Self::Done,
            StepStatus::Skipped => Self::Skipped,
            StepStatus::Failed => Self::Failed,
        }
    }
}

fn commit_line(step: &FinishCommit) -> String {
    match step.status {
        StepStatus::Ok => format!(
            "Committed {} on {} ({}).",
            files(step.files),
            step.branch,
            short(step.commit.as_deref())
        ),
        StepStatus::Skipped => format!("Nothing to commit on {}.", step.branch),
        StepStatus::Failed => format!(
            "Not committed: {}",
            step.message.as_deref().unwrap_or("git refused")
        ),
    }
}

fn merge_line(step: &FinishMerge) -> String {
    match step.status {
        StepStatus::Ok => format!(
            "{} {} into {} ({}).",
            match step.strategy {
                MergeStrategy::Merge => "Merged",
                MergeStrategy::Squash => "Squash-merged",
                MergeStrategy::Rebase => "Rebased and merged",
            },
            step.branch,
            step.target,
            short(step.commit.as_deref())
        ),
        StepStatus::Skipped if step.reason == Some(StepReason::NothingToMerge) => {
            format!(
                "Nothing to merge: {} already contains {}.",
                step.target, step.branch
            )
        }
        StepStatus::Skipped => "Not merged: the commit failed.".to_owned(),
        StepStatus::Failed if step.reason == Some(StepReason::Conflict) => format!(
            "Not merged: {} and {} conflict; the merge was undone and the commit stays on {}.",
            step.branch, step.target, step.branch
        ),
        StepStatus::Failed => format!(
            "Not merged: {}",
            step.message.as_deref().unwrap_or("git refused")
        ),
    }
}

fn push_line(step: Option<&FinishPush>) -> String {
    let Some(step) = step else {
        return "Not pushed: push was off. The commit stays local.".to_owned();
    };
    let upstream = step.upstream.as_deref().unwrap_or("its upstream");
    let why = step.message.as_deref().unwrap_or_default();
    match (step.status, step.reason) {
        (StepStatus::Ok, _) => format!(
            "Pushed {} to {upstream} ({}).",
            step.branch,
            short(step.commit.as_deref())
        ),
        (StepStatus::Skipped, Some(StepReason::UpToDate)) => {
            format!("Nothing to push: {upstream} is up to date.")
        }
        (StepStatus::Skipped, _) => "Not pushed: an earlier step failed.".to_owned(),
        (StepStatus::Failed, Some(StepReason::NoUpstream)) => {
            format!("Not pushed — no upstream: {why}")
        }
        (StepStatus::Failed, Some(StepReason::Rejected)) => {
            format!("Not pushed — rejected, nothing was forced: {why}")
        }
        (StepStatus::Failed, Some(StepReason::Auth)) => format!(
            "Not pushed — git could not sign in with your credentials (credential helper or SSH \
             agent): {why}"
        ),
        (StepStatus::Failed, _) => format!("Not pushed: {why}"),
    }
}

/// What a Finish did, one line per step, each with how it went.
pub fn step_lines(done: &WorktreeFinished) -> Vec<(StepTone, String)> {
    let mut lines = vec![(done.commit.status.into(), commit_line(&done.commit))];
    if let Some(merge) = &done.merge {
        lines.push((merge.status.into(), merge_line(merge)));
    }
    match &done.push {
        Some(push) => lines.push((push.status.into(), push_line(Some(push)))),
        None => lines.push((StepTone::Skipped, push_line(None))),
    }
    lines
}

/// The notice after a Finish: the first line that failed, else the last that did something.
fn notice(done: &WorktreeFinished) -> String {
    let lines = step_lines(done);
    lines
        .iter()
        .find(|(tone, _)| *tone == StepTone::Failed)
        .or_else(|| lines.iter().rev().find(|(tone, _)| *tone == StepTone::Done))
        .or(lines.last())
        .map(|(_, line)| line.clone())
        .unwrap_or_default()
}

impl ChangesView {
    // ---- read access (also for tests) -----------------------------------------------------

    /// Whether Finish can run here: the daemon knows `worktree.finish` and the checkout is on a
    /// branch.
    pub fn can_finish(&self, cx: &App) -> bool {
        self.env.sessions.read(cx).connection().supports_finish()
            && self.info.as_ref().is_some_and(|i| i.branch.is_some())
            && !self.finish.as_ref().is_some_and(|f| f.running)
    }

    pub fn finish_open(&self) -> bool {
        self.finish.is_some()
    }

    pub fn finish_strategy(&self) -> Option<MergeStrategy> {
        self.finish.as_ref().map(|f| f.strategy)
    }

    /// Whether the open form pushes.
    pub fn finish_push(&self) -> Option<bool> {
        self.finish.as_ref().map(|f| f.push)
    }

    pub fn finish_message(&self) -> Option<String> {
        self.finish.as_ref().map(|f| f.message.text().to_owned())
    }

    /// The daemon's refusal of the last Finish, while the form is open.
    pub fn finish_error(&self) -> Option<&str> {
        self.finish.as_ref().and_then(|f| f.error.as_deref())
    }

    /// What the last Finish in this view did, until dismissed.
    pub fn finished(&self) -> Option<&WorktreeFinished> {
        self.finished.as_ref()
    }

    /// What the message starts as: the subject names the files the commit takes
    /// ([`commit_subject`]; a main checkout commits its uncommitted files, a worktree delivers
    /// everything listed), and the body has one [`agent_line`] per task whose turns changed files
    /// here, the most recent change first (a task's work is open only after a turn that changed
    /// files, FX-REVIEW): the task the view was opened for or the worktree's branch names, or
    /// one whose chat ran in this checkout. No model is asked.
    fn finish_prefill(&self, cx: &App) -> String {
        let listed = self.files();
        let mut files: Vec<&DiffFile> = if self.can_merge() {
            listed.iter().collect()
        } else {
            listed.iter().filter(|file| file.uncommitted).collect()
        };
        if files.is_empty() {
            files = listed.iter().collect();
        }
        let subject = commit_subject(&files);
        let store = self.env.sessions.read(cx);
        let here = |session: SessionId| {
            store
                .session(session)
                .and_then(|s| s.placement.as_ref())
                .is_some_and(|p| p.root == self.canonical || p.root == self.path)
        };
        let named = self.task(cx).map(|task| task.id);
        let lines: Vec<String> = store
            .tasks()
            .open_work(|task| {
                Some(task.id) == named || task.sessions.iter().any(|session| here(*session))
            })
            .into_iter()
            .map(|task| {
                let session = task.sessions.last().and_then(|id| store.session(*id));
                let agent = session
                    .and_then(|s| {
                        s.chat
                            .as_ref()
                            .map(|chat| chat.agent.clone())
                            .or_else(|| s.agent.as_ref().map(|report| report.agent.clone()))
                    })
                    .or_else(|| match &task.run {
                        Some(TaskRun::Chat { agent, .. }) => Some(agent.clone()),
                        _ => None,
                    });
                // A chat's title is what the user called it (its first prompt, or a rename); a
                // task that ran a command is named by the task.
                let title = session
                    .filter(|s| s.chat.is_some())
                    .map_or(task.title.as_str(), |s| s.title.as_str());
                agent_line(agent.as_deref().map(harness_label).as_deref(), title)
            })
            .collect();
        if lines.is_empty() {
            subject
        } else {
            format!("{subject}\n\n{}", lines.join("\n"))
        }
    }

    // ---- actions --------------------------------------------------------------------------

    /// "Finish…": opens the form, the message prefilled.
    pub fn open_finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_finish(cx) {
            return;
        }
        self.delivery.close();
        if self.finish.is_none() {
            let prefilled = self.finish_prefill(cx);
            let mut message = TextInput::new();
            message.set(&prefilled);
            self.finish = Some(FinishForm {
                strategy: MergeStrategy::Merge,
                message,
                prefilled,
                vim: Vim::new(),
                push: true,
                push_chosen: false,
                running: false,
                error: None,
            });
            self.load_push_default(cx);
        }
        window.focus(&self.message_focus, cx);
        cx.notify();
    }

    /// A new listing while the form is open: a message the user has not edited follows it (the
    /// form may have opened before the files were listed).
    pub(super) fn refill_finish(&mut self, cx: &App) {
        if !self
            .finish
            .as_ref()
            .is_some_and(|f| !f.running && f.message.text() == f.prefilled)
        {
            return;
        }
        let prefilled = self.finish_prefill(cx);
        if let Some(form) = &mut self.finish
            && form.prefilled != prefilled
        {
            form.message.set(&prefilled);
            form.prefilled = prefilled;
        }
    }

    /// Reads `[finish] push` so the form shows what the daemon will do.
    fn load_push_default(&mut self, cx: &mut Context<Self>) {
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_daemon_settings() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(settings) = connection.daemon_settings().await else {
                return;
            };
            let push = settings
                .entries
                .iter()
                .find(|e| e.key == PUSH_SETTING)
                .and_then(|e| e.value.as_bool());
            this.update(cx, |this, cx| {
                if let (Some(push), Some(form)) = (push, &mut this.finish)
                    && !form.push_chosen
                {
                    form.push = push;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn close_finish(&mut self, cx: &mut Context<Self>) {
        if self.finish.as_ref().is_some_and(|f| f.running) {
            return;
        }
        self.finish = None;
        cx.notify();
    }

    pub fn set_finish_strategy(&mut self, strategy: MergeStrategy, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.finish {
            form.strategy = strategy;
            form.error = None;
            cx.notify();
        }
    }

    pub fn set_finish_message(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.finish {
            form.message.set(text);
            cx.notify();
        }
    }

    /// Push or keep it local, for this Finish.
    pub fn set_finish_push(&mut self, push: bool, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.finish {
            form.push = push;
            form.push_chosen = true;
            cx.notify();
        }
    }

    /// Dismisses the result of the last Finish.
    pub fn dismiss_finished(&mut self, cx: &mut Context<Self>) {
        self.finished = None;
        cx.notify();
    }

    /// Sends `worktree.finish`. The answer replaces the form with its steps (and, after a
    /// merge, the offer to remove the worktree); a refusal stays in the form.
    pub fn confirm_finish(&mut self, cx: &mut Context<Self>) {
        let worktree = self.can_merge();
        let Some(form) = &mut self.finish else {
            return;
        };
        if form.running {
            return;
        }
        form.running = true;
        form.error = None;
        let request = WorktreeFinish {
            checkout: self.path.clone(),
            message: form.message.text().trim().to_owned(),
            push: form.push_chosen.then_some(form.push),
            into: if worktree { self.base.clone() } else { None },
            strategy: form.strategy,
        };
        let finishing = self
            .env
            .sessions
            .update(cx, |store, cx| store.finish_checkout(request, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = finishing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(done) => {
                        this.finish = None;
                        cx.emit(ChangesEvent::Notice(notice(&done)));
                        if let Some(merge) =
                            done.merge.as_ref().filter(|m| m.status == StepStatus::Ok)
                        {
                            this.merged = Some(Merged {
                                result: WorktreeMerged {
                                    commit: merge.commit.clone().unwrap_or_default(),
                                    target: merge.target.clone(),
                                    branch: merge.branch.clone(),
                                    strategy: merge.strategy,
                                    source_dirty: merge.source_dirty,
                                },
                                removing: false,
                                error: None,
                            });
                        }
                        this.finished = Some(done);
                        this.reload(cx);
                    }
                    Err(error) => {
                        if let Some(form) = &mut this.finish {
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

    fn on_finish_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        match event.keystroke.key.as_str() {
            "escape" => {
                // Under Vim mode the first Escape leaves Insert (or Visual); Normal mode closes.
                if vim.enabled
                    && let Some(form) = &mut self.finish
                    && vim_ui::apply(
                        &mut form.vim,
                        &mut form.message,
                        event,
                        vim.options(false, true),
                        cx,
                    ) == Outcome::Handled
                {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                self.close_finish(cx);
            }
            "enter" if event.keystroke.modifiers.platform => self.confirm_finish(cx),
            _ => {
                let Some(form) = &mut self.finish else {
                    return;
                };
                if !edit_text(&mut form.message, &mut form.vim, vim, event, true, cx) {
                    return;
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// How the message's element reaches its text: the platform's text input and the mouse.
    fn finish_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.message_focus.clone(),
            |this, _, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(form) = &mut this.finish {
                    f(&mut FieldMut {
                        input: &mut form.message,
                        vim: vim_on.then_some(&mut form.vim),
                        multiline: true,
                    });
                }
            },
        )
    }

    // ---- drawing --------------------------------------------------------------------------

    pub(super) fn render_finish_form(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(form) = &self.finish else {
            return div().into_any_element();
        };
        let worktree = self.can_merge();
        let branch = self
            .info
            .as_ref()
            .and_then(|i| i.branch.clone())
            .unwrap_or_default();
        let target = self.base().unwrap_or("main").to_owned();
        let (title, hint) = if worktree {
            (
                format!("Finish {branch}: commit, merge into {target}, push"),
                format!(
                    "Commits every change in the worktree on {branch}, merges {branch} into \
                     {target} in the main checkout, then pushes {target} to its upstream with your \
                     own git credentials. Refused with nothing changed when the main checkout has \
                     uncommitted changes or another branch checked out; a conflict is undone. \
                     Never force-pushed, never rebased for you; git hooks do not run."
                ),
            )
        } else {
            (
                format!("Finish {branch}: commit and push"),
                format!(
                    "Commits every change listed here, untracked files included, as one commit \
                     on {branch}, then pushes {branch} to its upstream with your own git \
                     credentials. A branch behind its upstream is not pushed. Never \
                     force-pushed; git hooks do not run."
                ),
            )
        };
        let focused = self.message_focus.is_focused(window);
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
                    .child(SharedString::from(hint)),
            );
        if worktree {
            let mut strategies = div().flex().gap(px(tokens.spacing));
            for (strategy, label, key) in [
                (MergeStrategy::Merge, "Merge commit", "strategy-merge"),
                (MergeStrategy::Squash, "Squash", "strategy-squash"),
                (MergeStrategy::Rebase, "Rebase", "strategy-rebase"),
            ] {
                strategies = strategies.child(
                    chip(
                        SharedString::from(format!("changes-{key}")),
                        label,
                        form.strategy == strategy,
                        theme,
                        tokens,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_finish_strategy(strategy, cx)),
                    )
                    .child(probe(&self.env.regions, Region::Changes(key))),
                );
            }
            body = body
                .child(strategies)
                .child(self.render_merge_takes(theme, tokens, cx));
        }
        body = body.child(
            field_box(
                "changes-finish-message",
                &form.message,
                vim.then_some(&form.vim),
                if worktree && form.strategy == MergeStrategy::Squash {
                    "Commit message (also the squash commit's)"
                } else {
                    "Commit message"
                },
                focused,
                true,
                Some(self.finish_field(cx)),
                theme,
                tokens,
            )
            .relative()
            .track_focus(&self.message_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.on_finish_key(event, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    // The workbench takes focus on this same mouse down; ask again after it.
                    let handle = this.message_focus.clone();
                    cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                }),
            )
            .child(probe(&self.env.regions, Region::Changes("finish-message"))),
        );
        let push_target = if worktree { &target } else { &branch };
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    chip("changes-finish-push", "Push", form.push, theme, tokens)
                        .on_click(cx.listener({
                            let next = !form.push;
                            move |this, _, _, cx| this.set_finish_push(next, cx)
                        }))
                        .child(probe(&self.env.regions, Region::Changes("finish-push"))),
                )
                .child(
                    div()
                        .text_color(theme.muted)
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(if form.push {
                            format!("{push_target} goes to its upstream after the commit.")
                        } else {
                            "The commit stays local.".to_owned()
                        })),
                ),
        );
        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone()))
                    .relative()
                    .child(probe(&self.env.regions, Region::Changes("finish-error"))),
            );
        }
        body.child(
            div()
                .flex()
                .justify_end()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    ui::Button::new("changes-finish-cancel", "Cancel", theme, tokens)
                        .ghost()
                        .disabled(form.running)
                        .on_click(cx.listener(|this, _, _, cx| this.close_finish(cx)))
                        .child(probe(&self.env.regions, Region::Changes("finish-cancel"))),
                )
                .child(
                    ui::Button::new(
                        "changes-finish-confirm",
                        if form.running {
                            "Finishing…"
                        } else {
                            "Finish"
                        },
                        theme,
                        tokens,
                    )
                    .primary()
                    .loading(form.running)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_finish(cx)))
                    .child(probe(&self.env.regions, Region::Changes("finish-confirm"))),
                ),
        )
        .into_any_element()
    }

    /// What the last Finish did: one line per step, a failure in the danger colour with its
    /// reason, and the files of a conflict.
    pub(super) fn render_finished(
        &self,
        done: &WorktreeFinished,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let conflicts = done
            .merge
            .as_ref()
            .map(|m| m.conflicts.clone())
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing))
            .mx(px(tokens.spacing * 2.0))
            .mt(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .relative()
            .child(probe(&self.env.regions, Region::Changes("finish-result")))
            .children(step_lines(done).into_iter().map(|(tone, line)| {
                div()
                    .text_color(match tone {
                        StepTone::Done => theme.success,
                        StepTone::Skipped => theme.muted,
                        StepTone::Failed => theme.danger,
                    })
                    .child(SharedString::from(line))
            }))
            .when(!conflicts.is_empty(), |s| {
                s.child(
                    div()
                        .flex()
                        .flex_col()
                        .font_family(Look::of(cx).mono.clone())
                        .text_size(px(tokens.secondary_font_size))
                        .children(conflicts.into_iter().map(|file| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .child(crate::icons::glyph(crate::icons::Icon::StateWarning))
                                .child(SharedString::from(file))
                        })),
                )
            })
            .child(
                div().flex().justify_end().child(
                    ui::Button::new("changes-finish-done", "Done", theme, tokens)
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_finished(cx)))
                        .child(probe(&self.env.regions, Region::Changes("finish-done"))),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(status: StepStatus, reason: Option<StepReason>, message: Option<&str>) -> FinishPush {
        FinishPush {
            status,
            reason,
            message: message.map(str::to_owned),
            branch: "main".into(),
            upstream: Some("origin/main".into()),
            commit: Some("9d8c7b6a5f4e3d2c".into()),
            ahead: Some(1),
            behind: Some(0),
        }
    }

    fn committed() -> FinishCommit {
        FinishCommit {
            status: StepStatus::Ok,
            reason: None,
            message: None,
            branch: "main".into(),
            commit: Some("7a6b5c4d3e2f1098".into()),
            files: 2,
        }
    }

    #[test]
    fn each_step_says_what_happened_and_failures_say_why() {
        let done = WorktreeFinished {
            commit: committed(),
            merge: None,
            push: Some(push(StepStatus::Ok, None, None)),
        };
        assert_eq!(
            step_lines(&done),
            [
                (
                    StepTone::Done,
                    "Committed 2 files on main (7a6b5c4).".to_owned()
                ),
                (
                    StepTone::Done,
                    "Pushed main to origin/main (9d8c7b6).".to_owned()
                ),
            ]
        );
        assert_eq!(notice(&done), "Pushed main to origin/main (9d8c7b6).");

        for (reason, words) in [
            (StepReason::NoUpstream, "no upstream"),
            (StepReason::Rejected, "rejected, nothing was forced"),
            (StepReason::Auth, "could not sign in"),
        ] {
            let done = WorktreeFinished {
                commit: committed(),
                merge: None,
                push: Some(push(StepStatus::Failed, Some(reason), Some("git's words"))),
            };
            let lines = step_lines(&done);
            assert_eq!(lines[1].0, StepTone::Failed);
            assert!(
                lines[1].1.contains(words) && lines[1].1.ends_with("git's words"),
                "{lines:?}"
            );
            assert_eq!(notice(&done), lines[1].1, "the failure is the notice");
        }

        let local = WorktreeFinished {
            commit: committed(),
            merge: None,
            push: None,
        };
        assert_eq!(
            step_lines(&local)[1].1,
            "Not pushed: push was off. The commit stays local."
        );
    }

    fn file(path: &str, status: DiffStatus) -> DiffFile {
        DiffFile {
            path: path.into(),
            old_path: None,
            status,
            additions: 1,
            deletions: 0,
            binary: false,
            committed: false,
            uncommitted: true,
            staged: false,
            unstaged: true,
        }
    }

    fn subject(files: &[DiffFile]) -> String {
        commit_subject(&files.iter().collect::<Vec<_>>())
    }

    #[test]
    fn the_subject_names_what_the_commit_changes_in_gits_words() {
        use DiffStatus::{Added, Deleted, Modified, Renamed};
        assert_eq!(subject(&[]), "");
        assert_eq!(subject(&[file("app.py", Modified)]), "Update app.py");
        assert_eq!(
            subject(&[file("app.py", Modified), file("src/store.rs", Added)]),
            "Update app.py and store.rs"
        );
        assert_eq!(
            subject(&[
                file("src/a/mod.rs", Modified),
                file("src/b/mod.rs", Modified)
            ]),
            "Update src/a/mod.rs and src/b/mod.rs"
        );
        assert_eq!(
            subject(&[
                file("src/a.rs", Modified),
                file("src/b.rs", Added),
                file("src/c/d.rs", Renamed),
                file("src/e.rs", Deleted),
            ]),
            "Update 4 files in src/"
        );
        assert_eq!(
            subject(&[
                file("app.py", Modified),
                file("src/b.rs", Modified),
                file("web/c.js", Modified),
            ]),
            "Update 3 files"
        );
        // Only new (untracked) files are added; only removed ones deleted.
        assert_eq!(subject(&[file("todo.md", Added)]), "Add todo.md");
        assert_eq!(
            subject(&[
                file("docs/a.md", Added),
                file("docs/b/c.md", Added),
                file("docs/d.md", Added)
            ]),
            "Add 3 files in docs/"
        );
        assert_eq!(subject(&[file("old.txt", Deleted)]), "Delete old.txt");
    }

    #[test]
    fn a_body_line_names_the_harness_and_the_chat() {
        assert_eq!(
            agent_line(Some("OMP"), "Fix the greeting"),
            "Agent: OMP — Fix the greeting"
        );
        assert_eq!(agent_line(None, "Fix login"), "Task: Fix login");
    }
}
