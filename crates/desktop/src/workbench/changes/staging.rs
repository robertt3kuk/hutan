//! Staging in the Changes view (STAGING, protocol 0.53, capability `worktree.stage`, ADR 0020
//! § Amendment): whole files are staged and unstaged through the daemon (`worktree.stage`,
//! `worktree.unstage`), and what is staged is committed alone (*Commit staged…*, the Finish
//! form's *Commit staged only*). The view only sends requests and reads the listing again; git's
//! index is the daemon's.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_protocol::worktree::{DiffFile, WorktreeIndexChange};

use super::{ChangesEvent, ChangesView};
use crate::theme::{Theme, Tokens};
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui;
use crate::workbench::view::Region;

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
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                .cursor_pointer()
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
