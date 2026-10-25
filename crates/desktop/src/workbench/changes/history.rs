//! The Changes view's **History** segment (GIT-HISTORY): the recent commits of the checkout, each
//! with its message, its files and the diff of the one selected, drawn as the rest of the view
//! draws a diff. The daemon answers (`git.log` for the checkout, paged; `git.show` for a commit's
//! files, and for one file's patch); the segment is read-only like the others and sits beside
//! *Committed · All · Staged · Unstaged*.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use hutan_protocol::git::{GitCommit, GitLog, GitShow, GitShowResult};
use hutan_protocol::worktree::DiffFile;

use super::{ChangesView, status_mark};
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::{Look, counts_of};
use crate::workbench::history_ui::{self, Patch};
use crate::workbench::render::probe;
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;

/// Commits one request asks for.
const PAGE: u32 = 100;

/// What the History segment holds.
#[derive(Debug, Default)]
pub(super) struct HistoryState {
    /// The segment is the one shown.
    pub(super) on: bool,
    /// The commits were asked for at least once.
    started: bool,
    commits: Vec<GitCommit>,
    more: bool,
    next_before: Option<String>,
    loading: bool,
    error: Option<String>,
    /// Bumped by every reload: answers of older ones are dropped.
    generation: u64,
    selected: Option<String>,
    /// A commit's message and files (no patch), once read.
    shown: HashMap<String, GitShowResult>,
    /// The file of the selected commit whose diff shows.
    file: Option<String>,
    patches: HashMap<(String, String), Patch>,
}

impl ChangesView {
    // ---- read access (also for tests) -----------------------------------------------------

    /// Whether the History segment is the one shown.
    pub fn history_shown(&self) -> bool {
        self.history.on
    }

    /// The recent commits of the checkout, newest first, as far as they were read.
    pub fn history_commits(&self) -> &[GitCommit] {
        &self.history.commits
    }

    pub fn history_more(&self) -> bool {
        self.history.more
    }

    pub fn history_loading(&self) -> bool {
        self.history.loading
    }

    pub fn history_error(&self) -> Option<&str> {
        self.history.error.as_deref()
    }

    /// The selected commit.
    pub fn history_selected(&self) -> Option<&GitCommit> {
        let id = self.history.selected.as_deref()?;
        self.history.commits.iter().find(|c| c.id == id)
    }

    /// The files the selected commit changed, once read.
    pub fn history_files(&self) -> &[DiffFile] {
        self.history
            .selected
            .as_ref()
            .and_then(|id| self.history.shown.get(id))
            .map_or(&[], |shown| shown.files.as_slice())
    }

    /// The whole message of the selected commit, once read.
    pub fn history_message(&self) -> Option<&str> {
        let shown = self.history.shown.get(self.history.selected.as_ref()?)?;
        Some(&shown.message)
    }

    /// The file of the selected commit whose diff shows.
    pub fn history_file(&self) -> Option<&str> {
        self.history.file.as_deref()
    }

    /// The parsed diff of that file, once its patch arrived.
    pub fn history_diff(&self) -> Option<&hutan_chat_view::diff::FileDiff> {
        let key = (self.history.selected.clone()?, self.history.file.clone()?);
        match self.history.patches.get(&key)? {
            Patch::Ready { diff, .. } => Some(diff),
            _ => None,
        }
    }

    // ---- loading --------------------------------------------------------------------------

    /// Shows the History segment (reading the commits the first time).
    pub fn show_history(&mut self, cx: &mut Context<Self>) {
        if !self.history.on {
            self.history.on = true;
            if !self.history.started {
                self.reload_history(cx);
            }
            cx.notify();
        }
    }

    /// Leaves the History segment.
    pub fn hide_history(&mut self, cx: &mut Context<Self>) {
        if self.history.on {
            self.history.on = false;
            cx.notify();
        }
    }

    /// Reads the commits again from the newest one (the Refresh button, a window that came back,
    /// an agent's turn that ended). The selected commit stays while it is still listed.
    pub(super) fn reload_history(&mut self, cx: &mut Context<Self>) {
        self.history.generation += 1;
        self.history.started = true;
        self.fetch_history_page(None, cx);
    }

    /// Reads the next page of older commits.
    pub fn load_more_history(&mut self, cx: &mut Context<Self>) {
        if self.history.more && !self.history.loading {
            let before = self.history.next_before.clone();
            self.fetch_history_page(before, cx);
        }
    }

    fn fetch_history_page(&mut self, before: Option<String>, cx: &mut Context<Self>) {
        let generation = self.history.generation;
        self.history.loading = true;
        let reload = before.is_none();
        let request = GitLog {
            path: self.path.clone(),
            rev: None,
            limit: PAGE,
            before,
        };
        let asking = self
            .env
            .sessions
            .update(cx, |store, cx| store.git_log(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                if this.history.generation != generation {
                    return;
                }
                this.history.loading = false;
                match answer {
                    Ok(log) => {
                        this.history.error = None;
                        if reload {
                            this.history.commits = log.commits;
                            // What was read of the old list may be stale; the commits are not,
                            // but their files are asked for again when they are selected.
                        } else {
                            this.history.commits.extend(log.commits);
                        }
                        this.history.more = log.more;
                        this.history.next_before = log.next_before;
                        let keep = this
                            .history
                            .selected
                            .clone()
                            .filter(|id| this.history.commits.iter().any(|c| &c.id == id));
                        let pick =
                            keep.or_else(|| this.history.commits.first().map(|c| c.id.clone()));
                        match pick {
                            Some(id) => this.select_commit(&id, cx),
                            None => {
                                this.history.selected = None;
                                this.history.file = None;
                            }
                        }
                    }
                    Err(error) => this.history.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Shows a commit: its message and files (read the first time) and the diff of its first
    /// file.
    pub fn select_commit(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.history.commits.iter().any(|c| c.id == id) {
            return;
        }
        let changed = self.history.selected.as_deref() != Some(id);
        self.history.selected = Some(id.to_owned());
        if changed {
            self.history.file = None;
        }
        if let Some(shown) = self.history.shown.get(id) {
            if self.history.file.is_none() {
                let first = shown.files.first().map(|f| f.path.clone());
                if let Some(first) = first {
                    self.select_history_file(&first, cx);
                }
            }
            cx.notify();
            return;
        }
        let generation = self.history.generation;
        let request = GitShow {
            path: self.path.clone(),
            rev: id.to_owned(),
            old_path: None,
            patch: false,
        };
        let asking = self
            .env
            .sessions
            .update(cx, |store, cx| store.git_show(request, cx));
        let key = id.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                if this.history.generation != generation {
                    return;
                }
                match answer {
                    Ok(shown) => {
                        this.history.shown.insert(key.clone(), shown);
                        if this.history.selected.as_deref() == Some(key.as_str())
                            && this.history.file.is_none()
                        {
                            let first = this
                                .history
                                .shown
                                .get(&key)
                                .and_then(|s| s.files.first())
                                .map(|f| f.path.clone());
                            if let Some(first) = first {
                                this.select_history_file(&first, cx);
                            }
                        }
                    }
                    Err(error) => this.history.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Shows the diff of `file` in the selected commit (fetched the first time).
    pub fn select_history_file(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(commit) = self.history.selected.clone() else {
            return;
        };
        let Some(entry) = self
            .history
            .shown
            .get(&commit)
            .and_then(|s| s.files.iter().find(|f| f.path == file))
            .cloned()
        else {
            return;
        };
        self.history.file = Some(file.to_owned());
        let key = (commit.clone(), file.to_owned());
        if self.history.patches.contains_key(&key) {
            cx.notify();
            return;
        }
        self.history.patches.insert(key.clone(), Patch::Loading);
        let generation = self.history.generation;
        let root: PathBuf = self.path.clone();
        let request = GitShow {
            path: root.join(&entry.path),
            rev: commit,
            old_path: entry.old_path.as_ref().map(|old| root.join(old)),
            patch: true,
        };
        let asking = self
            .env
            .sessions
            .update(cx, |store, cx| store.git_show(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                if this.history.generation != generation {
                    return;
                }
                let patch = match answer {
                    Ok(shown) => Patch::of(&shown),
                    Err(error) => Patch::Failed(error),
                };
                this.history.patches.insert(key, patch);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- drawing --------------------------------------------------------------------------

    /// The segment's body: the commits on the left, the selected commit on the right.
    pub(super) fn render_history(
        &self,
        look: &Look,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        if let Some(error) = &self.history.error
            && self.history.commits.is_empty()
        {
            return ui::empty_state(
                ui::Mascot::Error,
                format!("Cannot show the history: {error}"),
                None,
                theme,
                tokens,
            )
            .flex_1()
            .into_any_element();
        }
        if self.history.commits.is_empty() {
            return div()
                .flex_1()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child(if self.history.loading || !self.history.started {
                    "Reading the history…"
                } else {
                    "This checkout has no commits yet."
                })
                .into_any_element();
        }
        let list = self.render_commit_list(theme, tokens, narrow, cx);
        let detail = self.render_commit(look, cx);
        let body = div().flex_1().min_h_0().flex();
        if narrow {
            body.flex_col().child(list).child(detail).into_any_element()
        } else {
            body.child(list).child(detail).into_any_element()
        }
    }

    fn render_commit_list(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let mut list = div()
            .id("changes-commits")
            .flex()
            .flex_col()
            .flex_none()
            .overflow_y_scroll()
            .border_color(theme.border)
            .py(px(tokens.spacing));
        list = if narrow {
            // The list takes 45 % of the panel's height (six rows at least), the commit the rest.
            list.w_full()
                .h(gpui::relative(0.45))
                .min_h(px(tokens.sidebar_row * 6.0))
                .border_b_1()
        } else {
            list.w(px(tokens.font_size * 30.0)).h_full().border_r_1()
        };
        for (index, commit) in self.history.commits.iter().enumerate() {
            let selected = self.history.selected.as_deref() == Some(commit.id.as_str());
            let id = commit.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("changes-commit-{index}")))
                    .relative()
                    .flex()
                    .flex_col()
                    .mx(px(tokens.spacing))
                    .px(px(tokens.spacing * 1.5))
                    .py(px(tokens.spacing * 0.75))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_commit(&id, cx)))
                    .child(
                        div()
                            .truncate()
                            .when(selected, |s| s.medium())
                            .child(SharedString::from(commit.subject.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(tokens.spacing * 1.5))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(
                                div()
                                    .id(SharedString::from(format!("changes-commit-when-{index}")))
                                    .flex_none()
                                    .tooltip(history_ui::when_tooltip(commit))
                                    .child(SharedString::from(history_ui::when(commit))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(commit.author.clone())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .child(SharedString::from(history_ui::short(&commit.id))),
                            ),
                    )
                    .child(probe(&self.env.regions, Region::ChangesCommit(index))),
            );
        }
        if self.history.more {
            list = list.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .child(
                        ui::Button::new(
                            "changes-history-more",
                            if self.history.loading {
                                "Reading…"
                            } else {
                                "Older commits"
                            },
                            theme,
                            tokens,
                        )
                        .compact()
                        .ghost()
                        .disabled(self.history.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load_more_history(cx)))
                        .child(probe(&self.env.regions, Region::Changes("history-more"))),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_commit(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let mut pane = div()
            .id("changes-commit-detail")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(tokens.spacing * 2.0))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .child(probe(&self.env.regions, Region::Changes("history-detail")));
        let Some(commit) = self.history_selected() else {
            return pane.into_any_element();
        };
        let shown = self.history.shown.get(&commit.id);
        let body = shown
            .map(|s| {
                s.message
                    .split_once('\n')
                    .map_or("", |(_, rest)| rest.trim())
                    .to_owned()
            })
            .filter(|text| !text.is_empty());
        pane = pane.child(
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 0.5))
                .child(
                    div()
                        .medium()
                        .child(SharedString::from(commit.subject.clone())),
                )
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(format!(
                            "{} · {} <{}> · {}{}",
                            history_ui::short(&commit.id),
                            commit.author,
                            commit.email,
                            hutan_client::time::local_time_text(commit.time.max(0) as u64 * 1000),
                            if commit.merge {
                                " · merge: files against the first parent"
                            } else {
                                ""
                            }
                        ))),
                )
                .when_some(body, |s, text| {
                    s.child(div().pt(px(tokens.spacing)).child(SharedString::from(text)))
                }),
        );
        let Some(shown) = shown else {
            return pane
                .child(div().text_color(theme.muted).child("Reading the commit…"))
                .into_any_element();
        };
        if shown.files.is_empty() {
            pane = pane.child(
                div()
                    .text_color(theme.muted)
                    .child("This commit changed no files."),
            );
        }
        let mut files = div().flex().flex_col();
        for (index, file) in shown.files.iter().enumerate() {
            let selected = self.history.file.as_deref() == Some(file.path.as_str());
            let (mark, color) = status_mark(file.status, theme);
            let path = file.path.clone();
            let name = match &file.old_path {
                Some(old) => format!("{old} → {}", file.path),
                None => file.path.clone(),
            };
            let hover = theme.surface_hover;
            files = files.child(
                div()
                    .id(SharedString::from(format!("changes-commit-file-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .px(px(tokens.spacing * 1.5))
                    .h(px(tokens.sidebar_row))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_history_file(&path, cx)),
                    )
                    .child(div().flex_none().w(px(12.0)).text_color(color).child(mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
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
                    .child(probe(&self.env.regions, Region::ChangesHistoryFile(index))),
            );
        }
        pane = pane.child(files);
        if let Some(file) = &self.history.file
            && let Some(commit) = self.history.selected.clone()
        {
            let key = (commit, file.clone());
            pane = pane.child(history_ui::patch_block(
                "changes-history-diff",
                self.history.patches.get(&key),
                look,
                &self.diff_x,
            ));
        }
        if shown.truncated {
            pane = pane.child(
                div()
                    .text_color(theme.warning)
                    .text_size(px(tokens.secondary_font_size))
                    .child("This commit changed more files than are listed."),
            );
        }
        pane.into_any_element()
    }
}
