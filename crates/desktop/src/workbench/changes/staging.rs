//! Staging in the Changes view (STAGING, protocol 0.53, capability `worktree.stage`, ADR 0020
//! § Amendment): whole files are staged and unstaged through the daemon (`worktree.stage`,
//! `worktree.unstage`), and what is staged is committed alone (*Commit staged…*, the Finish
//! form's *Commit staged only*). The view only sends requests and reads the listing again; git's
//! index is the daemon's.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_chat_view::diff::{self, MAX_LINES};
use hutan_core::CheckpointId;
use hutan_protocol::worktree::{
    DiffFile, DiffHunk, FileHunks, HunkRef, WorktreeDiscard, WorktreeHunksRequest,
    WorktreeIndexChange,
};

use super::{ChangesEvent, ChangesView};
use crate::theme::{Theme, Tokens};
use crate::workbench::chat::{Look, diff_row};
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;

/// One hunk of the Stage pane, as [`ChangesView::hunk_rows`] lists them: the unstaged ones
/// first, then the staged ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkRow {
    /// In the index (*Unstage hunk*), else only in the working tree (*Stage hunk*, *Discard
    /// hunk…*).
    pub staged: bool,
    /// The daemon's id of the hunk (`worktree.hunks`).
    pub id: String,
    /// The `@@ -a,b +c,d @@` line.
    pub header: String,
}

/// A discard that waits for the user's yes.
#[derive(Debug)]
struct DiscardAsk {
    request: WorktreeDiscard,
    /// The question, shown with *Discard* and *Cancel*.
    question: String,
    /// What the Undo offer says afterwards: `the hunk of a.txt`.
    what: String,
}

/// The offer to put a discard back, from the checkpoint it took.
#[derive(Debug)]
struct UndoOffer {
    checkpoint: CheckpointId,
    text: String,
    running: bool,
}

/// What the Stage pane holds: the hunks of the selected file, which one has the keyboard's
/// focus, a discard being asked and the Undo offer after one.
#[derive(Debug)]
pub(super) struct StagingState {
    /// The diff pane shows the Stage pane (Stage mode) instead of the branch diff (Review).
    on: bool,
    /// The file `hunks` belong to.
    file: Option<String>,
    hunks: Option<FileHunks>,
    error: Option<String>,
    /// Bumped by every read of the hunks; older answers are dropped.
    generation: u64,
    /// Index into the unstaged hunks followed by the staged ones.
    focus: usize,
    confirm: Option<DiscardAsk>,
    undo: Option<UndoOffer>,
    scroll: ScrollHandle,
}

impl Default for StagingState {
    fn default() -> Self {
        Self {
            on: false,
            file: None,
            hunks: None,
            error: None,
            generation: 0,
            focus: 0,
            confirm: None,
            undo: None,
            scroll: ScrollHandle::new(),
        }
    }
}

impl ChangesView {
    /// Whether the daemon stages (capability `worktree.stage`).
    pub fn staging_supported(&self, cx: &App) -> bool {
        self.env
            .sessions
            .read(cx)
            .connection()
            .supports_worktree_stage()
    }

    /// Whether something is staged now (what *Commit staged* would take).
    pub fn has_staged(&self) -> bool {
        self.include_uncommitted && self.files().iter().any(|f| f.staged)
    }

    /// Whether the Finish form offers *Commit staged only*.
    pub(super) fn staged_choice(&self, cx: &App) -> bool {
        self.has_staged() && self.staging_supported(cx)
    }

    /// Stages (`stage`) or unstages the files `paths` (a renamed file with both its paths).
    pub fn change_files(&mut self, paths: Vec<String>, stage: bool, cx: &mut Context<Self>) {
        if paths.is_empty() || !self.staging_supported(cx) {
            return;
        }
        self.send_index_change(
            WorktreeIndexChange {
                checkout: self.path.clone(),
                paths,
                ..WorktreeIndexChange::default()
            },
            stage,
            cx,
        );
    }

    /// *Stage all* (`stage`) or *Unstage all*.
    pub fn change_all(&mut self, stage: bool, cx: &mut Context<Self>) {
        if !self.staging_supported(cx) {
            return;
        }
        self.send_index_change(
            WorktreeIndexChange {
                checkout: self.path.clone(),
                all: true,
                ..WorktreeIndexChange::default()
            },
            stage,
            cx,
        );
    }

    /// Toggles a file row's checkbox: unstaged changes are staged, a fully staged file is
    /// unstaged.
    pub fn toggle_file_staged(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.files().iter().find(|f| f.path == file) else {
            return;
        };
        let stage = entry.unstaged;
        let mut paths = vec![entry.path.clone()];
        paths.extend(entry.old_path.clone());
        self.change_files(paths, stage, cx);
    }

    fn send_index_change(
        &mut self,
        request: WorktreeIndexChange,
        stage: bool,
        cx: &mut Context<Self>,
    ) {
        let changing = self
            .env
            .sessions
            .update(cx, |store, cx| store.change_index(request, stage, cx));
        cx.spawn(async move |this, cx| {
            let result = changing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.reload(cx),
                    Err(error) => cx.emit(ChangesEvent::Notice(format!(
                        "Cannot {} the changes: {error}",
                        if stage { "stage" } else { "unstage" }
                    ))),
                }
                // Hunk ids change with the index: read them again, also after a refusal.
                this.refresh_hunks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

// ---- the Stage pane: hunks ------------------------------------------------------------------

impl ChangesView {
    /// The diff pane shows the Stage pane (Stage mode, when the file can be staged) instead of
    /// the branch diff.
    pub fn stage_mode(&self) -> bool {
        self.staging.on
    }

    /// The selected file's row of the listing.
    fn selected_entry(&self) -> Option<&DiffFile> {
        let selected = self.selected.as_deref()?;
        self.files().iter().find(|f| f.path == selected)
    }

    /// Whether the diff pane offers Review · Stage: the daemon stages, the list has the
    /// uncommitted changes and the selected file has some.
    pub fn stage_available(&self, cx: &App) -> bool {
        self.staging_supported(cx)
            && self.include_uncommitted
            && self
                .selected_entry()
                .is_some_and(|f| f.staged || f.unstaged)
    }

    /// Switches the diff pane between Review (the branch diff, where comments hang) and Stage
    /// (the hunks of the selected file against the index, with *Stage hunk*, *Unstage hunk* and
    /// *Discard hunk…*).
    pub fn set_stage_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if on && !self.staging_supported(cx) {
            return;
        }
        self.staging.on = on;
        self.staging.confirm = None;
        self.refresh_hunks(cx);
        cx.notify();
    }

    /// Reads the hunks of the selected file again while the Stage pane is on.
    pub(super) fn refresh_hunks(&mut self, cx: &mut Context<Self>) {
        if !self.staging.on {
            return;
        }
        let Some(file) = self.selected.clone() else {
            return;
        };
        if self.staging.file.as_deref() != Some(file.as_str()) {
            self.staging.file = Some(file.clone());
            self.staging.hunks = None;
            self.staging.focus = 0;
            self.staging.confirm = None;
        }
        self.staging.generation += 1;
        let generation = self.staging.generation;
        let request = WorktreeHunksRequest {
            path: self.path.clone(),
            files: vec![file.clone()],
        };
        let reading = self
            .env
            .sessions
            .update(cx, |store, cx| store.worktree_hunks(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = reading.await;
            this.update(cx, |this, cx| {
                if this.staging.generation != generation {
                    return;
                }
                match answer {
                    Ok(listed) => {
                        this.staging.hunks = Some(
                            listed
                                .files
                                .into_iter()
                                .find(|f| f.path == file)
                                .unwrap_or_else(|| FileHunks {
                                    path: file.clone(),
                                    binary: false,
                                    unstaged: Vec::new(),
                                    staged: Vec::new(),
                                }),
                        );
                        this.staging.error = None;
                        let last = this.hunk_rows().len().saturating_sub(1);
                        this.staging.focus = this.staging.focus.min(last);
                    }
                    Err(error) => this.staging.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The hunks of the Stage pane, unstaged first, then staged.
    pub fn hunk_rows(&self) -> Vec<HunkRow> {
        let Some(file) = &self.staging.hunks else {
            return Vec::new();
        };
        let row = |staged: bool| {
            move |hunk: &DiffHunk| HunkRow {
                staged,
                id: hunk.id.clone(),
                header: hunk.header.clone(),
            }
        };
        file.unstaged
            .iter()
            .map(row(false))
            .chain(file.staged.iter().map(row(true)))
            .collect()
    }

    /// The hunk the keys `s`, `u` and `x` act on, while the Stage pane has any.
    pub fn hunk_focus(&self) -> Option<usize> {
        (self.staging.focus < self.hunk_rows().len()).then_some(self.staging.focus)
    }

    /// Why the hunks could not be read, while that is so.
    pub fn stage_error(&self) -> Option<&str> {
        self.staging.error.as_deref()
    }

    /// Focuses hunk `index` of [`hunk_rows`](Self::hunk_rows).
    pub fn focus_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.hunk_rows().len() {
            return;
        }
        self.staging.focus = index;
        self.staging.scroll.scroll_to_item(self.pane_child(index));
        cx.notify();
    }

    /// `j` (`forward`) and `k`: the next and the previous hunk.
    pub fn move_hunk_focus(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.hunk_rows().len();
        if count == 0 {
            return;
        }
        let next = if forward {
            (self.staging.focus + 1).min(count - 1)
        } else {
            self.staging.focus.saturating_sub(1)
        };
        self.focus_hunk(next, cx);
    }

    /// Where hunk `index`'s card is among the Stage pane's children (the section labels count).
    fn pane_child(&self, index: usize) -> usize {
        let unstaged = self
            .staging
            .hunks
            .as_ref()
            .map_or(0, |file| file.unstaged.len());
        if index < unstaged {
            1 + index
        } else {
            let staged_label = if unstaged > 0 { unstaged + 2 } else { 1 };
            staged_label + (index - unstaged)
        }
    }

    /// Hunk `index` as a request names it, and whether it is staged.
    fn hunk_ref(&self, index: usize) -> Option<(bool, HunkRef)> {
        let file = self.staging.hunks.as_ref()?;
        let (staged, hunk) = match file.unstaged.get(index) {
            Some(hunk) => (false, hunk),
            None => (true, file.staged.get(index - file.unstaged.len())?),
        };
        Some((
            staged,
            HunkRef {
                path: file.path.clone(),
                hunk_id: hunk.id.clone(),
                lines: Vec::new(),
            },
        ))
    }

    /// *Stage hunk* (`s`): an unstaged hunk goes into the index; the working tree is not touched.
    pub fn stage_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        self.change_hunk(index, true, cx);
    }

    /// *Unstage hunk* (`u`): a staged hunk leaves the index.
    pub fn unstage_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        self.change_hunk(index, false, cx);
    }

    fn change_hunk(&mut self, index: usize, stage: bool, cx: &mut Context<Self>) {
        if !self.staging_supported(cx) {
            return;
        }
        let Some((staged, hunk)) = self.hunk_ref(index) else {
            return;
        };
        if staged == stage {
            return;
        }
        self.send_index_change(
            WorktreeIndexChange {
                checkout: self.path.clone(),
                hunks: vec![hunk],
                ..WorktreeIndexChange::default()
            },
            stage,
            cx,
        );
    }

    // ---- discard, with a question first and Undo after ---------------------------------------

    /// *Discard hunk…* (`x`): asks first. Only an unstaged hunk can be discarded; the working
    /// tree gets the index's lines back.
    pub fn ask_discard_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((false, hunk)) = self.hunk_ref(index) else {
            return;
        };
        let header = self
            .hunk_rows()
            .get(index)
            .map(|row| row.header.clone())
            .unwrap_or_default();
        self.staging.confirm = Some(DiscardAsk {
            question: format!(
                "Discard the hunk {header} of {}? It is taken out of the working tree.",
                hunk.path
            ),
            what: format!("a hunk of {}", hunk.path),
            request: WorktreeDiscard {
                checkout: self.path.clone(),
                hunks: vec![hunk],
                ..WorktreeDiscard::default()
            },
        });
        cx.notify();
    }

    /// *Discard file…*: asks first. The unstaged changes of the file go (an untracked file is
    /// deleted); what is staged stays.
    pub fn ask_discard_file(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().filter(|f| f.unstaged) else {
            return;
        };
        let mut paths = vec![entry.path.clone()];
        paths.extend(entry.old_path.clone());
        let name = entry.path.clone();
        self.staging.confirm = Some(DiscardAsk {
            question: format!(
                "Discard the unstaged changes of {name}? They are taken out of the working tree \
                 (a new file is deleted)."
            ),
            what: format!("the changes of {name}"),
            request: WorktreeDiscard {
                checkout: self.path.clone(),
                paths,
                ..WorktreeDiscard::default()
            },
        });
        cx.notify();
    }

    /// The question of the discard being asked.
    pub fn discard_question(&self) -> Option<&str> {
        self.staging
            .confirm
            .as_ref()
            .map(|ask| ask.question.as_str())
    }

    /// The user said no.
    pub fn cancel_discard(&mut self, cx: &mut Context<Self>) {
        if self.staging.confirm.take().is_some() {
            cx.notify();
        }
    }

    /// The user said yes: the daemon takes a checkpoint, discards, and the Undo offer appears.
    pub fn confirm_discard(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.staging.confirm.take() else {
            return;
        };
        let what = ask.what;
        let discarding = self
            .env
            .sessions
            .update(cx, |store, cx| store.discard_worktree(ask.request, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = discarding.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(done) => {
                        this.staging.undo = Some(UndoOffer {
                            checkpoint: done.checkpoint.id,
                            text: format!("Discarded {what}."),
                            running: false,
                        });
                    }
                    Err(error) => {
                        cx.emit(ChangesEvent::Notice(format!("Cannot discard: {error}")));
                    }
                }
                this.reload(cx);
                this.refresh_hunks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the Undo offer after a discard says, while there is one.
    pub fn undo_offer(&self) -> Option<&str> {
        self.staging.undo.as_ref().map(|offer| offer.text.as_str())
    }

    /// *Undo*: restores the checkpoint the discard took.
    pub fn undo_discard(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.staging.undo.as_mut().filter(|offer| !offer.running) else {
            return;
        };
        offer.running = true;
        let checkpoint = offer.checkpoint;
        let undoing = self
            .env
            .sessions
            .update(cx, |store, cx| store.undo_discard(checkpoint, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = undoing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.staging.undo = None;
                        cx.emit(ChangesEvent::Notice("The discard was undone.".to_owned()));
                    }
                    Err(error) => {
                        if let Some(offer) = this.staging.undo.as_mut() {
                            offer.running = false;
                        }
                        cx.emit(ChangesEvent::Notice(format!(
                            "Cannot undo the discard: {error}"
                        )));
                    }
                }
                this.reload(cx);
                this.refresh_hunks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drops the Undo offer.
    pub fn dismiss_undo(&mut self, cx: &mut Context<Self>) {
        if self.staging.undo.take().is_some() {
            cx.notify();
        }
    }

    /// The Stage pane's keys, while the view has the keyboard: `j`/`k` (or the arrows) move
    /// between hunks, `s` stages, `u` unstages, `x` asks to discard the focused hunk; while a
    /// discard is asked Return confirms and Esc cancels. Returns whether the key was taken.
    pub(super) fn staging_key(&mut self, key: &str, plain: bool, cx: &mut Context<Self>) -> bool {
        if !self.staging.on || !plain || !self.stage_available(cx) {
            return false;
        }
        if self.staging.confirm.is_some() {
            return match key {
                "enter" => {
                    self.confirm_discard(cx);
                    true
                }
                "escape" => {
                    self.cancel_discard(cx);
                    true
                }
                _ => false,
            };
        }
        let Some(focus) = self.hunk_focus() else {
            return false;
        };
        match key {
            "j" | "down" => self.move_hunk_focus(true, cx),
            "k" | "up" => self.move_hunk_focus(false, cx),
            "s" => self.stage_hunk(focus, cx),
            "u" => self.unstage_hunk(focus, cx),
            "x" => self.ask_discard_hunk(focus, cx),
            _ => return false,
        }
        true
    }
}

impl ChangesView {
    /// The staging checkbox of a file row: checked when the file is staged, a dash when it is
    /// staged and changed again, empty when it is only unstaged; none for a committed-only file
    /// or a daemon that cannot stage.
    pub(super) fn render_file_check(
        &self,
        index: usize,
        file: &DiffFile,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.staging_supported(cx) || !(file.staged || file.unstaged) {
            return None;
        }
        let (glyph, tip) = match (file.staged, file.unstaged) {
            (true, false) => ("✓", "Unstage"),
            (true, true) => ("–", "Stage the rest"),
            _ => ("", "Stage"),
        };
        let path = file.path.clone();
        Some(
            div()
                .id(SharedString::from(format!("changes-check-{index}")))
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(theme.border)
                .when(file.staged, |s| {
                    s.bg(theme.accent).text_color(theme.on_accent())
                })
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(SharedString::from(tip))).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_file_staged(&path, cx);
                }))
                .child(glyph)
                .child(probe(&self.env.regions, Region::ChangesFileCheck(index)))
                .into_any_element(),
        )
    }

    /// The toolbar's staging controls: *Stage all* while something is unstaged, *Unstage all*
    /// and *Commit staged…* while something is staged.
    pub(super) fn render_staging_controls(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut controls = Vec::new();
        if !self.staging_supported(cx) || !self.include_uncommitted {
            return controls;
        }
        if self.files().iter().any(|f| f.unstaged) {
            controls.push(
                ui::Button::new("changes-stage-all", "Stage all", theme, tokens)
                    .compact()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.change_all(true, cx)))
                    .child(probe(&self.env.regions, Region::Changes("stage-all")))
                    .into_any_element(),
            );
        }
        if self.has_staged() {
            controls.push(
                ui::Button::new("changes-unstage-all", "Unstage all", theme, tokens)
                    .compact()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.change_all(false, cx)))
                    .child(probe(&self.env.regions, Region::Changes("unstage-all")))
                    .into_any_element(),
            );
            controls.push(
                ui::Button::new("changes-commit-staged", "Commit staged…", theme, tokens)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_commit_staged(window, cx)),
                    )
                    .child(probe(&self.env.regions, Region::Changes("commit-staged")))
                    .into_any_element(),
            );
        }
        controls
    }
}

// ---- the Stage pane: drawing ----------------------------------------------------------------

impl ChangesView {
    /// The right half of the view: the diff pane, with *Review · Stage* above it while the
    /// selected file can be staged, and the Stage pane in place of the diff in Stage mode.
    pub(super) fn render_diff_side(
        &self,
        look: &Look,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = self.stage_available(cx);
        let showing = available && self.staging.on;
        let mut side = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        if available {
            side = side.child(self.render_mode_bar(showing, theme, tokens, cx));
        }
        if showing && let Some(ask) = &self.staging.confirm {
            side = side.child(self.render_discard_ask(ask, theme, tokens, cx));
        }
        side.child(if showing {
            self.render_stage_pane(look, theme, tokens, cx)
        } else {
            self.render_patch(look, window, cx)
        })
        .into_any_element()
    }

    /// Hands the keyboard back to the view after a click in the pane, so `s`, `u`, `x`, `j`
    /// and `k` keep working.
    fn stage_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.review.focus, cx);
    }

    /// *Review · Stage*, and in Stage mode the file's own actions and the keys.
    fn render_mode_bar(
        &self,
        showing: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let segment = |id: &'static str, label: &'static str, on: bool, stage: bool| {
            ui::segment(id, label, showing == on, theme, tokens)
                .relative()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_stage_mode(stage, cx);
                    window.focus(&this.review.focus, cx);
                }))
                .child(probe(
                    &self.env.regions,
                    Region::Changes(if stage { "mode-stage" } else { "mode-review" }),
                ))
        };
        let mut bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .pt(px(tokens.spacing * 2.0))
            .child(
                ui::segmented(theme, tokens)
                    .child(segment("changes-mode-review", "Review", false, false))
                    .child(segment("changes-mode-stage", "Stage", true, true)),
            );
        if showing && let Some(entry) = self.selected_entry() {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("j k move · s stage · u unstage · x discard"),
            );
            if entry.unstaged {
                bar = bar.child(
                    ui::Button::new("changes-stage-file", "Stage file", theme, tokens)
                        .compact()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.stage_click(window, cx);
                            if let Some(file) = this.selected.clone() {
                                this.stage_selected_file(&file, true, cx);
                            }
                        }))
                        .child(probe(&self.env.regions, Region::Changes("stage-file"))),
                );
            }
            if entry.staged {
                bar = bar.child(
                    ui::Button::new("changes-unstage-file", "Unstage file", theme, tokens)
                        .compact()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.stage_click(window, cx);
                            if let Some(file) = this.selected.clone() {
                                this.stage_selected_file(&file, false, cx);
                            }
                        }))
                        .child(probe(&self.env.regions, Region::Changes("unstage-file"))),
                );
            }
            if entry.unstaged {
                bar = bar.child(
                    ui::Button::new("changes-discard-file", "Discard file…", theme, tokens)
                        .compact()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.stage_click(window, cx);
                            this.ask_discard_file(cx);
                        }))
                        .child(probe(&self.env.regions, Region::Changes("discard-file"))),
                );
            }
        }
        bar.into_any_element()
    }

    /// Stages (`stage`) or unstages the whole selected file, a renamed one with both paths.
    fn stage_selected_file(&mut self, file: &str, stage: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.files().iter().find(|f| f.path == file) else {
            return;
        };
        let mut paths = vec![entry.path.clone()];
        paths.extend(entry.old_path.clone());
        self.change_files(paths, stage, cx);
    }

    /// The question before a discard: *Cancel* (Esc) and *Discard* (Return).
    fn render_discard_ask(
        &self,
        ask: &DiscardAsk,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .mx(px(tokens.spacing * 2.0))
            .mt(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 2.0))
            .card(theme, tokens)
            .child(div().flex_1().min_w_0().child(SharedString::from(format!(
                "{} Undo is offered afterwards.",
                ask.question
            ))))
            .child(
                ui::Button::new("changes-discard-cancel", "Cancel", theme, tokens)
                    .ghost()
                    .shortcut("esc")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.stage_click(window, cx);
                        this.cancel_discard(cx);
                    }))
                    .child(probe(&self.env.regions, Region::Changes("discard-cancel"))),
            )
            .child(
                ui::Button::new("changes-discard-confirm", "Discard", theme, tokens)
                    .destructive()
                    .shortcut("⏎")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.stage_click(window, cx);
                        this.confirm_discard(cx);
                    }))
                    .child(probe(&self.env.regions, Region::Changes("discard-confirm"))),
            )
            .child(probe(&self.env.regions, Region::Changes("discard-ask")))
            .into_any_element()
    }

    /// After a discard: what went, and *Undo*.
    pub(super) fn render_undo_offer(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let offer = self.staging.undo.as_ref()?;
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .mx(px(tokens.spacing * 2.0))
                .mt(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing * 2.0))
                .card(theme, tokens)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(offer.text.clone())),
                )
                .child(
                    ui::Button::new("changes-undo-discard", "Undo", theme, tokens)
                        .compact()
                        .loading(offer.running)
                        .on_click(cx.listener(|this, _, _, cx| this.undo_discard(cx)))
                        .child(probe(&self.env.regions, Region::Changes("undo-discard"))),
                )
                .child(
                    ui::Button::new("changes-undo-dismiss", "Dismiss", theme, tokens)
                        .compact()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_undo(cx)))
                        .child(probe(&self.env.regions, Region::Changes("undo-dismiss"))),
                )
                .child(probe(&self.env.regions, Region::Changes("undo-offer")))
                .into_any_element(),
        )
    }

    /// The hunks of the selected file: the unstaged ones, then the staged ones, each with its
    /// own buttons.
    fn render_stage_pane(
        &self,
        look: &Look,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut pane = div()
            .id("changes-stage")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.staging.scroll)
            .p(px(tokens.spacing * 2.0))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0));
        let note = |text: String, color: gpui::Hsla| {
            div().text_color(color).child(SharedString::from(text))
        };
        match (&self.staging.hunks, &self.staging.error) {
            (None, Some(error)) => {
                pane = pane.child(note(
                    format!("Cannot read the hunks: {error}"),
                    theme.danger,
                ));
            }
            (None, None) => {
                pane = pane.child(note("Reading the hunks…".to_owned(), theme.muted));
            }
            (Some(file), _) if file.binary => {
                pane = pane.child(note(
                    "A binary file: it can be staged, unstaged or discarded as a whole, not by hunk."
                        .to_owned(),
                    theme.muted,
                ));
            }
            (Some(file), _) if file.unstaged.is_empty() && file.staged.is_empty() => {
                pane = pane.child(note(
                    "No hunks to stage (a mode change, or an empty file): use Stage file."
                        .to_owned(),
                    theme.muted,
                ));
            }
            (Some(file), _) => {
                let focus = self.hunk_focus();
                let mut index = 0;
                for (label, hunks, staged) in [
                    ("Unstaged", &file.unstaged, false),
                    ("Staged", &file.staged, true),
                ] {
                    if hunks.is_empty() {
                        continue;
                    }
                    pane = pane.child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(format!(
                                "{label} · {}",
                                match hunks.len() {
                                    1 => "1 hunk".to_owned(),
                                    n => format!("{n} hunks"),
                                }
                            ))),
                    );
                    for hunk in hunks {
                        pane = pane.child(self.render_hunk_card(
                            index,
                            hunk,
                            staged,
                            focus == Some(index),
                            look,
                            cx,
                        ));
                        index += 1;
                    }
                }
            }
        }
        if let Some(error) = &self.staging.error
            && self.staging.hunks.is_some()
        {
            pane = pane.child(note(
                format!("Cannot read the hunks again: {error}"),
                theme.warning,
            ));
        }
        pane.child(probe(&self.env.regions, Region::Changes("stage")))
            .into_any_element()
    }

    /// One hunk: its header with *Stage hunk* · *Discard hunk…* (unstaged) or *Unstage hunk*
    /// (staged), and its lines. The focused one has the accent outline.
    fn render_hunk_card(
        &self,
        index: usize,
        hunk: &DiffHunk,
        staged: bool,
        focused: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let parsed = diff::split_patch(&format!("{}\n{}", hunk.header, hunk.body))
            .into_iter()
            .next()
            .map(|d| d.lines)
            .unwrap_or_default();
        let mut lines = div()
            .id(SharedString::from(format!("changes-stage-lines-{index}")))
            .overflow_x_scroll()
            .flex()
            .flex_col()
            .items_start()
            .font_family(look.mono.clone())
            .text_size(px(tokens.font_size - 0.5))
            .whitespace_nowrap();
        for line in parsed.iter().skip(1).take(MAX_LINES) {
            lines = lines.child(diff_row(line, look));
        }
        if parsed.len() > MAX_LINES + 1 {
            lines = lines.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child(SharedString::from(format!(
                        "… the first {MAX_LINES} lines of the hunk are shown"
                    ))),
            );
        }
        let mut buttons = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing));
        if staged {
            buttons = buttons.child(
                ui::Button::new(
                    SharedString::from(format!("changes-unstage-hunk-{index}")),
                    "Unstage hunk",
                    theme,
                    tokens,
                )
                .compact()
                .ghost()
                .shortcut("u")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.stage_click(window, cx);
                    this.focus_hunk(index, cx);
                    this.unstage_hunk(index, cx);
                }))
                .child(probe(
                    &self.env.regions,
                    Region::ChangesStage(index, "unstage"),
                )),
            );
        } else {
            buttons = buttons
                .child(
                    ui::Button::new(
                        SharedString::from(format!("changes-stage-hunk-{index}")),
                        "Stage hunk",
                        theme,
                        tokens,
                    )
                    .compact()
                    .ghost()
                    .shortcut("s")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.stage_click(window, cx);
                        this.focus_hunk(index, cx);
                        this.stage_hunk(index, cx);
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::ChangesStage(index, "stage"),
                    )),
                )
                .child(
                    ui::Button::new(
                        SharedString::from(format!("changes-discard-hunk-{index}")),
                        "Discard hunk…",
                        theme,
                        tokens,
                    )
                    .compact()
                    .ghost()
                    .shortcut("x")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.stage_click(window, cx);
                        this.focus_hunk(index, cx);
                        this.ask_discard_hunk(index, cx);
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::ChangesStage(index, "discard"),
                    )),
                );
        }
        div()
            .id(SharedString::from(format!(
                "changes-stage-hunk-card-{index}"
            )))
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(if focused { theme.focus } else { theme.border })
            .bg(theme.bg)
            .overflow_hidden()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.stage_click(window, cx);
                this.focus_hunk(index, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens.spacing * 2.0))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .bg(theme.diff_hunk_bg)
                    .text_color(theme.diff_hunk_fg)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(look.mono.clone())
                            .text_size(px(tokens.font_size - 0.5))
                            .child(SharedString::from(hunk.header.clone())),
                    )
                    .child(buttons),
            )
            .child(lines)
            .child(probe(
                &self.env.regions,
                Region::ChangesStage(index, "card"),
            ))
            .into_any_element()
    }
}
