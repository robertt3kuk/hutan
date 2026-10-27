//! File history (GIT-HISTORY): *Show File History* (a file tab's menu, the Files panel's
//! context menu, the palette) opens a tab with the commits that changed the file — date, author,
//! message, following renames — and, for the one selected, its message and its diff for that file
//! drawn as the Changes view draws one. *Open at this revision* opens the file as it was in a
//! read-only tab ([`RevisionView`]).
//!
//! Everything comes from the daemon: `git.log` for the file (paged), `git.show` for the diff of a
//! commit limited to the file, `git.file` for a revision's text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use hutan_code_view::{CodeStyle, CodeView, FileBuffer};
use hutan_protocol::git::{GitCommit, GitFile, GitLog, GitShow, GitShowResult};

use super::chat::Look;
use super::history_ui::{self, Patch};
use super::registry::PanelEnv;
use super::render::probe;
use super::style::UiStyle;
use super::ui::{self, StyledExt as _};
use super::view::{Region, Workbench};
use crate::theme::DensityExt as _;

/// Commits one request of the history asks for.
const PAGE: u32 = 100;
/// Pages read to find a commit a click on a blame asked for before giving up.
const SEEK_PAGES: usize = 20;
/// The commit list's width in ems when there is room, and the narrowest it shrinks to.
const LIST_BASIS: f32 = 34.0;
const LIST_MIN: f32 = 20.0;
/// The narrowest the detail gets beside the list, in ems.
const DETAIL_MIN: f32 = 28.0;
/// Below this many ems the list stacks above the detail instead of sharing the width.
const STACK_BELOW: f32 = LIST_MIN + DETAIL_MIN;

/// What a file history tab asks of the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileHistoryEvent {
    /// A line for the notice area.
    Notice(String),
    /// *Open at this revision*: `path` is the file's name at that commit, `commit` the full hash.
    OpenRevision { path: PathBuf, commit: String },
}

/// The tab of one file's history.
pub struct FileHistoryView {
    env: PanelEnv,
    path: PathBuf,
    /// The checkout's top level, once the first answer is in.
    root: Option<PathBuf>,
    commits: Vec<GitCommit>,
    more: bool,
    next_before: Option<String>,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    /// A commit to select as soon as the list has it (a click on a line's blame).
    wanted: Option<String>,
    seeking: usize,
    patches: HashMap<String, Patch>,
    messages: HashMap<String, GitShowResult>,
    /// Bumped by every reload: answers of older ones are dropped.
    generation: u64,
    focus: FocusHandle,
    diff_x: ScrollHandle,
    list: ScrollHandle,
    /// The width the body was painted with (0 before the first paint).
    width: std::rc::Rc<std::cell::Cell<f32>>,
    /// The list is above the detail, as drawn last.
    stacked: bool,
}

impl EventEmitter<FileHistoryEvent> for FileHistoryView {}

impl std::fmt::Debug for FileHistoryView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileHistoryView")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Focusable for FileHistoryView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileHistoryView {
    pub fn new(
        env: PanelEnv,
        path: PathBuf,
        select: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            env,
            path,
            root: None,
            commits: Vec::new(),
            more: false,
            next_before: None,
            loading: false,
            error: None,
            selected: None,
            wanted: select,
            seeking: 0,
            patches: HashMap::new(),
            messages: HashMap::new(),
            generation: 0,
            focus: cx.focus_handle(),
            diff_x: ScrollHandle::new(),
            list: ScrollHandle::new(),
            width: std::rc::Rc::new(std::cell::Cell::new(0.0)),
            stacked: false,
        };
        view.reload(cx);
        view
    }

    // ---- read access (also for tests) -----------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `History · name`.
    pub fn title(&self) -> String {
        format!("History · {}", hutan_code_view::file_name(&self.path))
    }

    pub fn commits(&self) -> &[GitCommit] {
        &self.commits
    }

    pub fn more(&self) -> bool {
        self.more
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn selected(&self) -> Option<&GitCommit> {
        let id = self.selected.as_deref()?;
        self.commits.iter().find(|c| c.id == id)
    }

    /// The parsed diff of the selected commit's file, once it arrived.
    pub fn selected_diff(&self) -> Option<&hutan_chat_view::diff::FileDiff> {
        match self.patches.get(self.selected.as_deref()?)? {
            Patch::Ready { diff, .. } => Some(diff),
            _ => None,
        }
    }

    /// The selected commit's whole message, once it arrived.
    pub fn selected_message(&self) -> Option<&str> {
        let shown = self.messages.get(self.selected.as_deref()?)?;
        Some(&shown.message)
    }

    // ---- loading --------------------------------------------------------------------------

    /// Reads the history again from the newest commit (the Refresh button).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.seeking = 0;
        self.fetch_page(None, cx);
    }

    /// Reads the next page of older commits.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.more && !self.loading {
            let before = self.next_before.clone();
            self.fetch_page(before, cx);
        }
    }

    fn fetch_page(&mut self, before: Option<String>, cx: &mut Context<Self>) {
        let generation = self.generation;
        self.loading = true;
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
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match answer {
                    Ok(log) => {
                        this.error = None;
                        this.root = Some(log.root);
                        if reload {
                            this.commits.clear();
                            this.patches.clear();
                            this.messages.clear();
                        }
                        this.commits.extend(log.commits);
                        this.more = log.more;
                        this.next_before = log.next_before;
                        this.settle_selection(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// After a page: the commit that was asked for (reading further pages until it is found),
    /// else the one that was selected, else the newest.
    fn settle_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(wanted) = self.wanted.clone() {
            if self.commits.iter().any(|c| c.id == wanted) {
                self.wanted = None;
                self.select(&wanted, cx);
                return;
            }
            if self.more && self.seeking < SEEK_PAGES {
                self.seeking += 1;
                let before = self.next_before.clone();
                self.fetch_page(before, cx);
                return;
            }
            self.wanted = None;
        }
        let keep = self
            .selected
            .clone()
            .filter(|id| self.commits.iter().any(|c| &c.id == id));
        if let Some(id) = keep.or_else(|| self.commits.first().map(|c| c.id.clone())) {
            self.select(&id, cx);
        }
    }

    /// Shows a commit's diff for the file (fetched the first time).
    pub fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(commit) = self.commits.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        self.selected = Some(id.to_owned());
        if !self.patches.contains_key(id) {
            self.fetch_patch(&commit, cx);
        }
        cx.notify();
    }

    /// The file's absolute path as `commit` named it.
    fn path_at(&self, commit: &GitCommit) -> PathBuf {
        match (&self.root, &commit.path) {
            (Some(root), Some(name)) => root.join(name),
            _ => self.path.clone(),
        }
    }

    fn fetch_patch(&mut self, commit: &GitCommit, cx: &mut Context<Self>) {
        self.patches.insert(commit.id.clone(), Patch::Loading);
        let generation = self.generation;
        let id = commit.id.clone();
        let request = GitShow {
            path: self.path_at(commit),
            rev: commit.id.clone(),
            old_path: commit
                .old_path
                .as_ref()
                .zip(self.root.as_ref())
                .map(|(old, root)| root.join(old)),
            patch: true,
        };
        let asking = self
            .env
            .sessions
            .update(cx, |store, cx| store.git_show(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match answer {
                    Ok(shown) => {
                        this.patches.insert(id.clone(), Patch::of(&shown));
                        this.messages.insert(id, shown);
                    }
                    Err(error) => {
                        this.patches.insert(id, Patch::Failed(error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// *Open at this revision* for the selected commit.
    pub fn open_revision(&mut self, cx: &mut Context<Self>) {
        if let Some(commit) = self.selected().cloned() {
            let path = self.path_at(&commit);
            cx.emit(FileHistoryEvent::OpenRevision {
                path,
                commit: commit.id,
            });
        }
    }

    /// Copies the selected commit's hash.
    pub fn copy_hash(&mut self, cx: &mut Context<Self>) {
        if let Some(commit) = self.selected() {
            let id = commit.id.clone();
            cx.write_to_clipboard(ClipboardItem::new_string(id));
            cx.emit(FileHistoryEvent::Notice(
                "Copied the commit's hash".to_owned(),
            ));
        }
    }

    /// ↑ ↓ move the selection through the list.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let at = self
            .selected
            .as_deref()
            .and_then(|id| self.commits.iter().position(|c| c.id == id));
        let next = match (at, forward) {
            (Some(at), true) => (at + 1).min(self.commits.len().saturating_sub(1)),
            (Some(at), false) => at.saturating_sub(1),
            (None, _) => 0,
        };
        if let Some(id) = self.commits.get(next).map(|c| c.id.clone()) {
            self.select(&id, cx);
        }
        if forward && next + 1 >= self.commits.len() {
            self.load_more(cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" | "j" => self.step(true, cx),
            "up" | "k" => self.step(false, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for FileHistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let look = Look::of(cx);
        self.stacked = self.stacked_for(&tokens);

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .min_h(px(tokens.sidebar_row + 8.0))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .truncate()
                    .medium()
                    .child(SharedString::from(hutan_code_view::file_name(&self.path))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(self.path.display().to_string())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(match (self.commits.len(), self.more) {
                        (0, _) => String::new(),
                        (n, true) => format!("{n}+ commits"),
                        (1, false) => "1 commit".to_owned(),
                        (n, false) => format!("{n} commits"),
                    })),
            )
            .child(
                ui::IconButton::new(
                    "history-refresh",
                    crate::icons::Icon::ActionRefresh,
                    "Refresh",
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                .child(probe(&self.env.regions, Region::FileHistory("refresh"))),
            );

        let body = if let Some(error) = &self.error {
            ui::empty_state(
                ui::Mascot::Error,
                format!("Cannot show the history: {error}"),
                None,
                &theme,
                &tokens,
            )
            .flex_1()
            .into_any_element()
        } else if self.commits.is_empty() {
            div()
                .flex_1()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child(if self.loading {
                    "Reading the history…"
                } else {
                    "No commit has changed this file yet."
                })
                .into_any_element()
        } else {
            // The list shrinks to `LIST_MIN` ems, the detail never gets below `DETAIL_MIN`; with
            // less room than both the list stacks above the detail (the width of the frame is
            // measured by the canvas below, a frame on the other side of the line draws again).
            let stacked = self.stacked_for(&tokens);
            let width = std::rc::Rc::clone(&self.width);
            let (em, was_stacked) = (tokens.font_size, stacked);
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .when(stacked, |s| s.flex_col())
                .child(self.render_list(&theme, &tokens, stacked, cx))
                .child(self.render_detail(&look, stacked, cx))
                .child(
                    gpui::canvas(
                        |bounds, _, _| bounds,
                        move |_, bounds, window, cx| {
                            let measured: f32 = bounds.size.width.into();
                            width.set(measured);
                            if (measured < STACK_BELOW * em) != was_stacked {
                                cx.notify(window.current_view());
                            }
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .into_any_element()
        };

        div()
            .id("file-history")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .bg(theme.bg)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .child(probe(&self.env.regions, Region::FileHistory("root")))
            .child(header)
            .child(body)
    }
}

impl FileHistoryView {
    /// Whether the list goes above the detail: the measured width of the body is below
    /// [`STACK_BELOW`] ems.
    fn stacked_for(&self, tokens: &crate::theme::Tokens) -> bool {
        let measured = self.width.get();
        measured > 0.0 && measured < STACK_BELOW * tokens.font_size
    }

    /// Whether the list is above the detail (a narrow centre pane), as drawn last.
    pub fn stacked(&self) -> bool {
        self.stacked
    }

    fn render_list(
        &self,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        stacked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let mut list = div()
            .id("history-commits")
            .flex()
            .flex_col()
            .when(!stacked, |s| {
                s.flex_shrink(1.0)
                    .flex_basis(px(tokens.font_size * LIST_BASIS))
                    .min_w(px(tokens.font_size * LIST_MIN))
                    .max_w(px(tokens.font_size * LIST_BASIS))
                    .h_full()
                    .border_r_1()
            })
            .when(stacked, |s| {
                s.flex_none()
                    .w_full()
                    .max_h(px(tokens.font_size * 12.0))
                    .border_b_1()
            })
            .overflow_y_scroll()
            .track_scroll(&self.list)
            .border_color(theme.border)
            .py(px(tokens.spacing));
        for (index, commit) in self.commits.iter().enumerate() {
            let selected = self.selected.as_deref() == Some(commit.id.as_str());
            let id = commit.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("history-commit-{index}")))
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.select(&id, cx);
                    }))
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
                                    .id(SharedString::from(format!("history-when-{index}")))
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
                    .child(probe(&self.env.regions, Region::FileHistoryRow(index))),
            );
        }
        if self.more {
            list = list.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .child(
                        ui::Button::new(
                            "history-more",
                            if self.loading {
                                "Reading…"
                            } else {
                                "Older commits"
                            },
                            theme,
                            tokens,
                        )
                        .compact()
                        .ghost()
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
                        .child(probe(&self.env.regions, Region::FileHistory("more"))),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_detail(&self, look: &Look, stacked: bool, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let mut pane = div()
            .id("history-detail")
            .relative()
            .flex_1()
            .min_w(px(if stacked {
                0.0
            } else {
                tokens.font_size * DETAIL_MIN
            }))
            .min_h_0()
            .overflow_y_scroll()
            .p(px(tokens.spacing * 2.0))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 1.5))
            .child(probe(&self.env.regions, Region::FileHistory("detail")));
        let Some(commit) = self.selected() else {
            return pane.into_any_element();
        };
        let body = self
            .messages
            .get(&commit.id)
            .map(|shown| {
                shown
                    .message
                    .split_once('\n')
                    .map_or("", |(_, rest)| rest.trim())
                    .to_owned()
            })
            .filter(|text| !text.is_empty());
        pane = pane.child(
            div()
                .flex()
                .items_start()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
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
                                    "{} · {} <{}> · {}",
                                    history_ui::short(&commit.id),
                                    commit.author,
                                    commit.email,
                                    hutan_client::time::local_time_text(
                                        commit.time.max(0) as u64 * 1000
                                    )
                                ))),
                        )
                        .when_some(commit.old_path.clone(), |s, old| {
                            s.child(
                                div()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("Renamed from {old}"))),
                            )
                        })
                        .when_some(body, |s, text| {
                            s.child(
                                div()
                                    .pt(px(tokens.spacing))
                                    .text_color(theme.fg)
                                    .child(SharedString::from(text)),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(tokens.spacing))
                        .child(
                            ui::Button::new(
                                "history-open-revision",
                                "Open at this revision",
                                theme,
                                tokens,
                            )
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| this.open_revision(cx)))
                            .child(probe(
                                &self.env.regions,
                                Region::FileHistory("open-revision"),
                            )),
                        )
                        .child(
                            ui::Button::new("history-copy-hash", "Copy hash", theme, tokens)
                                .compact()
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| this.copy_hash(cx)))
                                .child(probe(&self.env.regions, Region::FileHistory("copy"))),
                        ),
                ),
        );
        pane = pane.child(history_ui::patch_block(
            "history-diff-block",
            self.patches.get(&commit.id),
            look,
            &self.diff_x,
        ));
        pane.into_any_element()
    }
}

// A file as it was ---------------------------------------------------------------------------

/// What a revision tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionState {
    Loading,
    Ready,
    /// Why there is no text to show.
    Unavailable(String),
}

/// The tab of a file at a commit: read-only text in a code view of a buffer of its own (not in
/// the window's buffer store: nothing watches it, saves it or hands it to a language server).
pub struct RevisionView {
    path: PathBuf,
    rev: String,
    buffer: Entity<FileBuffer>,
    code: Entity<CodeView>,
    state: RevisionState,
}

impl std::fmt::Debug for RevisionView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevisionView")
            .field("path", &self.path)
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

impl RevisionView {
    pub fn new(
        env: &PanelEnv,
        path: PathBuf,
        rev: String,
        style: CodeStyle,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| {
            let mut buffer = FileBuffer::new(path.clone(), cx);
            buffer.set_read_only(true, cx);
            buffer
        });
        let code = cx.new(|cx| CodeView::new(buffer.clone(), style, cx));
        let request = GitFile {
            path: path.clone(),
            rev: rev.clone(),
        };
        let asking = env
            .sessions
            .update(cx, |store, cx| store.git_file(request, cx));
        let view = Self {
            path,
            rev,
            buffer,
            code,
            state: RevisionState::Loading,
        };
        cx.spawn(async move |this, cx| {
            let answer = asking.await;
            this.update(cx, |this, cx| {
                this.state = match answer {
                    Ok(file) if file.binary => RevisionState::Unavailable(
                        "A binary file: there is no text to show.".into(),
                    ),
                    Ok(file) if file.too_large => RevisionState::Unavailable(format!(
                        "This file is {} at that revision: too large to show here.",
                        super::file_view::human_size(file.size)
                    )),
                    Ok(file) => {
                        this.buffer.update(cx, |buffer, cx| {
                            buffer.set_loaded(&file.content, false, cx);
                            buffer.set_syntax_active(true, cx);
                        });
                        RevisionState::Ready
                    }
                    Err(error) => RevisionState::Unavailable(format!(
                        "Cannot read the file at that revision: {error}"
                    )),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        view
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rev(&self) -> &str {
        &self.rev
    }

    pub fn state(&self) -> &RevisionState {
        &self.state
    }

    pub fn buffer(&self) -> &Entity<FileBuffer> {
        &self.buffer
    }

    pub fn code(&self) -> &Entity<CodeView> {
        &self.code
    }

    /// `name @ 1a2b3c4`.
    pub fn title(&self) -> String {
        format!(
            "{} @ {}",
            hutan_code_view::file_name(&self.path),
            history_ui::short(&self.rev)
        )
    }

    pub fn set_style(&mut self, style: CodeStyle, cx: &mut Context<Self>) {
        self.code.update(cx, |code, cx| code.set_style(style, cx));
    }
}

impl Render for RevisionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing * 0.5))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(SharedString::from(format!(
                "{} at commit {} · read-only",
                self.path.display(),
                history_ui::short(&self.rev)
            )));
        let body = match &self.state {
            RevisionState::Ready => self.code.clone().into_any_element(),
            RevisionState::Loading => div()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child("Reading the file…")
                .into_any_element(),
            RevisionState::Unavailable(why) => div()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child(SharedString::from(why.clone()))
                .into_any_element(),
        };
        div()
            .id("revision-view")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(bar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

// The workbench -------------------------------------------------------------------------------

impl Workbench {
    /// The history view of the file at `path`, made when there is none.
    pub(crate) fn history_view(
        &mut self,
        path: &str,
        select: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FileHistoryView> {
        if let Some(view) = self.history_views.get(path) {
            return view.clone();
        }
        let env = self.env.clone();
        let file = PathBuf::from(path);
        let view = cx.new(|cx| FileHistoryView::new(env, file, select, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &FileHistoryEvent, window, cx| match event {
                FileHistoryEvent::Notice(text) => this.set_notice(text.clone(), cx),
                FileHistoryEvent::OpenRevision { path, commit } => {
                    this.open_revision(path, commit, window, cx);
                }
            },
        );
        self.history_subscriptions
            .insert(path.to_owned(), subscription);
        self.history_views.insert(path.to_owned(), view.clone());
        view
    }

    /// The history view of `path`, if its tab was opened in this window.
    pub fn file_history(&self, path: &str) -> Option<Entity<FileHistoryView>> {
        self.history_views.get(path).cloned()
    }

    /// *Show File History*: the history of the file at `path` in a tab (or its tab focused); with
    /// `select`, the commit a click on a line's blame named is the one shown.
    pub fn open_file_history(
        &mut self,
        path: &Path,
        select: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.sessions.read(cx).connection().supports_git_history() {
            self.set_notice(
                "This daemon does not answer git history. Restart it to update.",
                cx,
            );
            return;
        }
        let key = path.display().to_string();
        let existing = self.history_views.contains_key(&key);
        let view = self.history_view(&key, select.clone(), window, cx);
        if existing {
            view.update(cx, |view, cx| {
                view.wanted = select;
                view.seeking = 0;
                if view.wanted.is_some() {
                    view.settle_selection(cx);
                }
            });
        }
        self.show_item(ItemRef::history(key), window, cx);
    }

    /// *Open at this revision*: the file as it was at `commit`, read-only, in a tab.
    pub fn open_revision(
        &mut self,
        path: &Path,
        commit: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (path.display().to_string(), commit.to_owned());
        if !self.revision_views.contains_key(&key) {
            let env = self.env.clone();
            let style = self.code_style(cx);
            let (file, rev) = (path.to_owned(), commit.to_owned());
            let view = cx.new(|cx| RevisionView::new(&env, file, rev, style, cx));
            self.revision_views.insert(key.clone(), view);
        }
        self.show_item(ItemRef::revision(key.0, key.1), window, cx);
    }

    /// The revision view of `path` at `commit`, if its tab was opened in this window.
    pub fn revision(&self, path: &str, commit: &str) -> Option<Entity<RevisionView>> {
        self.revision_views
            .get(&(path.to_owned(), commit.to_owned()))
            .cloned()
    }

    /// The tab of `item` focused, or added to the focused pane (replacing an empty
    /// placeholder).
    fn show_item(&mut self, item: ItemRef, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, index)) = self.state.find_item(&item) {
            self.try_mutate(cx, |s| {
                s.set_active_item(pane, index)?;
                s.focus_pane(pane)
            });
        } else {
            let pane = self.state.active_pane_id();
            let placeholder = self
                .state
                .pane(pane)
                .and_then(|p| match p.items.as_slice() {
                    [only @ ItemRef::Placeholder { .. }] => Some(only.clone()),
                    _ => None,
                });
            self.try_mutate(cx, |s| {
                if let Some(placeholder) = &placeholder
                    && s.replace_item(placeholder, item.clone())
                {
                    return s.focus_pane(pane);
                }
                s.add_item(pane, item.clone()).map(|_| ())
            });
        }
        self.focus_root(window, cx);
    }

    /// The body of a file history tab.
    pub(crate) fn render_history_item(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.history_view(path, None, window, cx);
        super::render::cached_view(&self.regions, view)
    }

    /// The body of a revision tab.
    pub(crate) fn render_revision_item(
        &mut self,
        path: &str,
        rev: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (path.to_owned(), rev.to_owned());
        if !self.revision_views.contains_key(&key) {
            let env = self.env.clone();
            let style = self.code_style(cx);
            let (file, commit) = (PathBuf::from(path), rev.to_owned());
            let view = cx.new(|cx| RevisionView::new(&env, file, commit, style, cx));
            self.revision_views.insert(key.clone(), view);
        }
        match self.revision_views.get(&key) {
            Some(view) => super::render::cached_view(&self.regions, view.clone()),
            None => div().into_any_element(),
        }
    }

    /// Drops the views of history and revision tabs that are gone.
    pub(crate) fn sync_history_views(&mut self) {
        let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut revisions: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        let mut keep = |item: &ItemRef| match item {
            ItemRef::History { path } => {
                files.insert(path.clone());
            }
            ItemRef::Revision { path, rev } => {
                revisions.insert((path.clone(), rev.clone()));
            }
            _ => {}
        };
        for pane in self.state.center.panes() {
            pane.items.iter().for_each(&mut keep);
        }
        if let Some(saved) = &self.state.stashed {
            for pane in saved.center.panes() {
                pane.items.iter().for_each(&mut keep);
            }
        }
        self.history_views.retain(|path, _| files.contains(path));
        self.history_subscriptions
            .retain(|path, _| files.contains(path));
        self.revision_views.retain(|key, _| revisions.contains(key));
    }

    /// The restyle of a theme or `editor` font change reaches revision tabs too.
    pub(crate) fn restyle_revisions(&mut self, style: &CodeStyle, cx: &mut Context<Self>) {
        for view in self.revision_views.values() {
            view.update(cx, |view, cx| view.set_style(style.clone(), cx));
        }
    }

    /// The file tab menu's *Show File History*.
    pub(crate) fn show_history_entry(path: &str) -> super::view::MenuEntry {
        let path = PathBuf::from(path);
        Self::entry("Show File History", move |this, window, cx| {
            this.open_file_history(&path, None, window, cx);
        })
    }

    /// The context menu of a Files panel row: the file's actions (*Show File History*), the file
    /// operations (EX-7: new file and folder, rename, duplicate, cut, copy, paste, Move to Trash)
    /// and its path. The row is the selected one: the right click selected it.
    pub(crate) fn show_files_menu(
        &mut self,
        at: &super::actions::FilesMenuAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::panels::FilesCommand as Cmd;
        let command = |label: &'static str, command: Cmd| {
            Self::entry(label, move |this, window, cx| {
                this.files_command(command, window, cx);
            })
        };
        let path = at.path.clone();
        let mut entries = Vec::new();
        if at.file {
            let open = path.clone();
            entries.push(Self::entry("Open", move |this, window, cx| {
                this.open_file(super::files::FileOpen::picked(open.clone()), window, cx);
            }));
            entries.push(Self::show_history_entry(&path));
        }
        entries.push(command("New File…", Cmd::NewFile));
        entries.push(command("New Folder…", Cmd::NewFolder));
        entries.push(command("Rename", Cmd::Rename));
        entries.push(command("Duplicate", Cmd::Duplicate));
        entries.push(command("Cut", Cmd::Cut));
        entries.push(command("Copy", Cmd::Copy));
        entries.push(command("Paste", Cmd::Paste));
        let copy = path;
        entries.push(Self::entry("Copy Path", move |this, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
            this.set_notice("Copied the path", cx);
        }));
        entries.push(command("Move to Trash", Cmd::Trash));
        self.open_menu(at.position, entries, window, cx);
    }

    /// *Show File History* for the file of the focused tab.
    pub(crate) fn show_active_file_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_center_file() {
            Some(path) => self.open_file_history(Path::new(&path), None, window, cx),
            None => self.set_notice("The focused tab is not a file", cx),
        }
    }
}

use super::model::ItemRef;
