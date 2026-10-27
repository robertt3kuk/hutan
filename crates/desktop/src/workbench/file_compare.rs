//! Compare tabs (EX-4): two files of the Files tree side by side as one unified diff
//! (*Select for Compare* then *Compare with Selected*), or a file against its version in HEAD
//! (*Compare with HEAD*). The diff is drawn with the history tab's rows (`history_ui::patch_block`,
//! the chat's diff rows under hunk headers), read-only.
//!
//! Both sides come through the daemon (`fs.read` as a picked file, `git.file` for HEAD), so the
//! tab works for a remote window too. The texts are read when the tab opens and again on
//! *Refresh*; each side is at most [`COMPARE_MAX_BYTES`].

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, AppContext as _, Context, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};
use hutan_chat_view::diff::{self, FileDiff};
use hutan_client::Connection;
use hutan_protocol::git::GitFile;

use super::chat::Look;
use super::history_ui::{self, Patch};
use super::model::ItemRef;
use super::registry::PanelEnv;
use super::style::UiStyle;
use super::ui;
use super::view::Workbench;
use crate::theme::DensityExt as _;

/// The largest text of a side the tab diffs.
pub const COMPARE_MAX_BYTES: usize = 2 << 20;

/// What one side of a compare is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    /// A file as it is on disk.
    File(PathBuf),
    /// A file as the last commit has it (`git.file` at `HEAD`).
    Head(PathBuf),
}

impl Side {
    fn path(&self) -> &Path {
        match self {
            Side::File(path) | Side::Head(path) => path,
        }
    }

    /// What the diff header and the tab say: the name, and `(HEAD)` for the committed one.
    fn label(&self) -> String {
        let name = hutan_code_view::file_name(self.path());
        match self {
            Side::File(_) => name,
            Side::Head(_) => format!("{name} (HEAD)"),
        }
    }
}

/// What a compare tab shows.
#[derive(Debug, Clone)]
pub enum CompareState {
    Loading,
    /// Both sides read and the texts are the same.
    Identical,
    /// Both sides read: the unified diff of left to right.
    Differs(Patch),
    /// Why a side could not be read or the sides cannot be compared.
    Failed(String),
}

/// The tab of a compare.
pub struct CompareView {
    env: PanelEnv,
    left: Side,
    right: Side,
    state: CompareState,
    /// Bumped by every read: an older answer is dropped.
    generation: u64,
    focus: FocusHandle,
    diff_x: ScrollHandle,
}

impl std::fmt::Debug for CompareView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompareView")
            .field("left", &self.left)
            .field("right", &self.right)
            .finish_non_exhaustive()
    }
}

impl Focusable for CompareView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// File text as the diff reads it: no byte-order mark, `\n` line breaks.
fn normalized(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

async fn read_side(connection: &Connection, side: &Side) -> Result<String, String> {
    let too_large = |side: &Side| {
        format!(
            "{} is over {} MiB: too large to compare here",
            side.label(),
            COMPARE_MAX_BYTES >> 20
        )
    };
    match side {
        Side::File(path) => {
            let text = connection
                .fs_read_all(path, true)
                .await
                .map_err(|error| super::files::error_text(&error))?
                .text;
            if text.len() > COMPARE_MAX_BYTES {
                return Err(too_large(side));
            }
            Ok(normalized(&text))
        }
        Side::Head(path) => {
            let file = connection
                .git_file(GitFile {
                    path: path.clone(),
                    rev: "HEAD".to_owned(),
                })
                .await
                .map_err(|error| super::files::error_text(&error))?;
            if file.binary {
                return Err(format!(
                    "{} is binary: there is no text to compare",
                    side.label()
                ));
            }
            if file.too_large || file.content.len() > COMPARE_MAX_BYTES {
                return Err(too_large(side));
            }
            Ok(normalized(&file.content))
        }
    }
}

/// The unified diff of `old` to `new`, read the way the history tab reads a commit's patch.
fn unified(left: &str, right: &str, old: &str, new: &str) -> Option<FileDiff> {
    if old == new {
        return None;
    }
    let patch = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(left, right)
        .to_string();
    diff::split_patch(&patch).into_iter().next()
}

impl CompareView {
    pub fn new(env: PanelEnv, left: Side, right: Side, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            env,
            left,
            right,
            state: CompareState::Loading,
            generation: 0,
            focus: cx.focus_handle(),
            diff_x: ScrollHandle::new(),
        };
        view.reload(cx);
        view
    }

    // ---- read access (also for tests) -----------------------------------------------------

    pub fn left(&self) -> &Side {
        &self.left
    }

    pub fn right(&self) -> &Side {
        &self.right
    }

    pub fn state(&self) -> &CompareState {
        &self.state
    }

    /// The diff, once both sides are read and differ.
    pub fn diff(&self) -> Option<&FileDiff> {
        match &self.state {
            CompareState::Differs(Patch::Ready { diff, .. }) => Some(diff),
            _ => None,
        }
    }

    /// `Compare · a.rs ↔ b.rs`, `Compare · a.rs (HEAD) ↔ a.rs`.
    pub fn title(&self) -> String {
        compare_title(&self.left, &self.right)
    }

    // ---- loading --------------------------------------------------------------------------

    /// Reads both sides again (the Refresh button).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.state = CompareState::Loading;
        let connection = self.env.sessions.read(cx).connection().clone();
        let (left, right) = (self.left.clone(), self.right.clone());
        let work = cx.background_spawn(async move {
            let old = read_side(&connection, &left).await?;
            let new = read_side(&connection, &right).await?;
            Ok::<_, String>(unified(&left.label(), &right.label(), &old, &new))
        });
        cx.spawn(async move |this, cx| {
            let answer = work.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.state = match answer {
                    Ok(Some(diff)) => CompareState::Differs(Patch::Ready {
                        diff,
                        truncated: false,
                        binary: false,
                    }),
                    Ok(None) => CompareState::Identical,
                    Err(why) => CompareState::Failed(why),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

fn compare_title(left: &Side, right: &Side) -> String {
    format!("Compare · {} ↔ {}", left.label(), right.label())
}

impl Render for CompareView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let look = Look::of(cx);
        let counts = self.diff().map(FileDiff::counts);
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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(format!(
                        "{} ↔ {} · read-only",
                        self.left.path().display(),
                        match &self.right {
                            Side::File(path) => path.display().to_string(),
                            Side::Head(_) => "working copy".to_owned(),
                        }
                    ))),
            )
            .children(counts.map(|counts| div().flex_none().child(SharedString::from(counts))))
            .child(
                ui::Button::new("compare-refresh", "Refresh", &theme, &tokens)
                    .compact()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            );
        let message = |text: String| -> AnyElement {
            div()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child(SharedString::from(text))
                .into_any_element()
        };
        let body: AnyElement = match &self.state {
            CompareState::Loading => message("Reading the files…".to_owned()),
            CompareState::Identical => message("The files are identical.".to_owned()),
            CompareState::Failed(why) => message(format!("Cannot compare: {why}")),
            CompareState::Differs(patch) => div()
                .id("compare-body")
                .size_full()
                .overflow_y_scroll()
                .p(px(tokens.spacing * 2.0))
                .child(history_ui::patch_block(
                    "compare-diff-block",
                    Some(patch),
                    &look,
                    &self.diff_x,
                ))
                .into_any_element(),
        };
        div()
            .id("compare-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(bar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Workbench {
    /// The compare view of the tab `item` (a [`ItemRef::Compare`] or [`ItemRef::CompareHead`]),
    /// made when there is none.
    pub(crate) fn compare_view(
        &mut self,
        item: &ItemRef,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<CompareView>> {
        if let Some(view) = self.compare_views.get(item) {
            return Some(view.clone());
        }
        let (left, right) = match item {
            ItemRef::Compare { left, right } => (
                Side::File(PathBuf::from(left)),
                Side::File(PathBuf::from(right)),
            ),
            ItemRef::CompareHead { path } => (
                Side::Head(PathBuf::from(path)),
                Side::File(PathBuf::from(path)),
            ),
            _ => return None,
        };
        let env = self.env.clone();
        let view = cx.new(|cx| CompareView::new(env, left, right, cx));
        self.compare_views.insert(item.clone(), view.clone());
        Some(view)
    }

    /// The file *Select for Compare* remembered.
    pub fn compare_selection(&self) -> Option<&Path> {
        self.compare_source.as_deref()
    }

    /// The compare view of a tab, if it was opened in this window (tests).
    pub fn compare(&self, item: &ItemRef) -> Option<gpui::Entity<CompareView>> {
        self.compare_views.get(item).cloned()
    }

    /// The body of a compare tab.
    pub(crate) fn render_compare_item(
        &mut self,
        item: &ItemRef,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.compare_view(item, cx) {
            Some(view) => super::render::cached_view(&self.regions, view),
            None => div().into_any_element(),
        }
    }

    /// Drops the views of compare tabs that are gone.
    pub(crate) fn sync_compare_views(&mut self) {
        let mut open: std::collections::HashSet<ItemRef> = std::collections::HashSet::new();
        let mut keep = |item: &ItemRef| {
            if matches!(item, ItemRef::Compare { .. } | ItemRef::CompareHead { .. }) {
                open.insert(item.clone());
            }
        };
        for pane in self.state.center.panes() {
            pane.items.iter().for_each(&mut keep);
        }
        if let Some(saved) = &self.state.stashed {
            for pane in saved.center.panes() {
                pane.items.iter().for_each(&mut keep);
            }
        }
        self.compare_views.retain(|item, _| open.contains(item));
    }

    /// *Compare with Selected*: `left` against `right`, two files, in a tab.
    pub fn open_compare(
        &mut self,
        left: &Path,
        right: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ItemRef::compare(left.display().to_string(), right.display().to_string());
        self.compare_view(&item, cx);
        self.show_item(item, window, cx);
    }

    /// *Compare with HEAD*: the file against its committed version, in a tab.
    pub fn open_compare_head(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemRef::compare_head(path.display().to_string());
        self.compare_view(&item, cx);
        self.show_item(item, window, cx);
    }
}

/// The title of a compare tab for its item (the tab strip and the palette).
pub(crate) fn item_title(item: &ItemRef) -> Option<String> {
    match item {
        ItemRef::Compare { left, right } => Some(compare_title(
            &Side::File(PathBuf::from(left)),
            &Side::File(PathBuf::from(right)),
        )),
        ItemRef::CompareHead { path } => Some(compare_title(
            &Side::Head(PathBuf::from(path)),
            &Side::File(PathBuf::from(path)),
        )),
        _ => None,
    }
}
