//! Drawing the Rewind preview sheet, the Checkpoints list and the "Restored … · Undo" toast
//! (`rewind.rs` has what they do). One primary button per surface (*Restore N files*), quiet
//! labels, and every state said in words, never by colour alone.

use std::path::Path;

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::*,
    px,
};
use hutan_core::{CheckpointKind, CheckpointState};
use hutan_protocol::checkpoint::restore_label;

use super::actions;
use super::chat::{Look, changed_text, diff_row};
use super::harnesses::harness_name;
use super::render::probe;
use super::rewind::{DiffState, Group, Phase, RewindHeader, SheetFocus};
use super::style::TextTooltip;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{self, Button, StyledExt as _};
use super::view::{Region, Workbench};
use super::workspaces::tilde;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// How tall the sheet's file list and diff grow before they scroll.
const LIST_MAX_HEIGHT: f32 = 220.0;
const DIFF_MAX_HEIGHT: f32 = 220.0;

/// A muted line of the sheet's header whose tooltip is the checkout's full path.
fn muted_with_path(
    id: &'static str,
    text: &str,
    path: &str,
    theme: &Theme,
    tokens: &Tokens,
) -> impl IntoElement {
    let path = SharedString::from(path.to_owned());
    div()
        .id(id)
        .text_size(px(tokens.secondary_font_size))
        .text_color(theme.muted)
        .child(SharedString::from(text.to_owned()))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(path.clone())).into())
}

impl Workbench {
    /// The actions the turn footers, the menus and the palette dispatch.
    pub(crate) fn bind_rewind_actions<E: InteractiveElement>(
        &self,
        root: E,
        cx: &mut Context<Self>,
    ) -> E {
        root.on_action(
            cx.listener(|this, a: &actions::RewindCheckpoint, window, cx| {
                this.open_rewind(a.0.session, a.0.checkpoint, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::UndoRestore, window, cx| {
            this.undo_restore(a.0.session, a.0.checkpoint, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowCheckpoints, window, cx| {
                this.show_checkpoints(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RewindFocusedChat, window, cx| {
                this.rewind_focused_chat(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::UndoLastRestore, window, cx| {
                this.undo_last_restore(window, cx);
            }),
        )
    }

    /// The names of `sessions`, for `shared with …` and the busy line.
    fn session_names(&self, sessions: &[hutan_core::SessionId], cx: &gpui::App) -> String {
        let store = self.sessions.read(cx);
        sessions
            .iter()
            .map(|id| {
                store
                    .session(*id)
                    .map(|s| s.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| id.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Where `checkout` is, said short: the name of the project whose folder it is, else the
    /// folder (`~/…` under the home folder).
    fn checkout_place(&self, checkout: &Path, cx: &gpui::App) -> String {
        self.sessions
            .read(cx)
            .workspaces()
            .iter()
            .find(|w| w.project.as_ref().is_some_and(|p| p.root == checkout))
            .map_or_else(|| tilde(checkout), |w| w.name.clone())
    }

    /// The open sheet's header once its plan is in: the facts line, the honest label, and the
    /// checkout's full path (their tooltip).
    pub fn rewind_sheet_header(&self, cx: &gpui::App) -> Option<RewindHeader> {
        let preview = self.rewind.sheet.as_ref()?.preview.as_ref()?;
        let agent = preview
            .agent
            .as_deref()
            .map_or_else(|| "The agent".to_owned(), harness_name);
        let turns = match preview.checkpoints.len() {
            1 => "1 turn".to_owned(),
            n => format!("{n} turns"),
        };
        let place = self.checkout_place(&preview.checkout, cx);
        Some(RewindHeader {
            facts: format!("{agent} · {turns} · {place}"),
            label: restore_label(
                preview.agent.as_deref(),
                "this checkout",
                preview.head_moved.as_ref(),
            ),
            checkout: preview.checkout.display().to_string(),
        })
    }

    pub(crate) fn render_rewind_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.rewind.sheet.is_some() {
            self.exits.rewind.cancel();
            return self.build_rewind_sheet(true, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.rewind.sheet,
            |this| &mut this.exits.rewind,
            |this| this.build_rewind_sheet(false, cx),
        )
        .flatten()
    }

    /// `wanted` is false while the sheet leaves: it takes no input.
    fn build_rewind_sheet(&mut self, wanted: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.rewind.sheet.as_ref()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let look = Look::of(cx);
        let regions = self.regions.clone();
        let focus = sheet.focus;

        let title = if sheet.undo {
            "Undo the restore"
        } else {
            "Rewind files to before this turn"
        };
        let header = self.rewind_sheet_header(cx);
        let mut shared = None;
        let mut busy = None;
        if let Some(preview) = &sheet.preview {
            if !preview.shared_with.is_empty() {
                shared = Some(format!(
                    "Shared with {} — their files are left alone",
                    self.session_names(&preview.shared_with, cx)
                ));
            }
            if !preview.busy.is_empty() {
                busy = Some(format!(
                    "{} has a turn running in this checkout: nothing can be restored until it ends.",
                    self.session_names(&preview.busy, cx)
                ));
            }
        }
        let harness = sheet
            .preview
            .as_ref()
            .and_then(|p| p.agent.as_deref())
            .map_or_else(|| "the agent".to_owned(), harness_name);

        let button = |id: &'static str,
                      label: SharedString,
                      kind: ui::ButtonKind,
                      enabled: bool,
                      at: SheetFocus| {
            Button::new(id, label, &theme, &tokens)
                .kind(kind)
                .disabled(!enabled)
                .ring(focus == at)
        };

        // The files, by group.
        let mut list = div()
            .id("rewind-files")
            .flex()
            .flex_col()
            .max_h(px(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .text_size(px(tokens.font_size));
        let mut last_group = None;
        for (index, row) in sheet.rows.iter().enumerate() {
            if last_group != Some(row.group) {
                last_group = Some(row.group);
                let count = sheet.rows.iter().filter(|r| r.group == row.group).count();
                list = list.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing * 1.5))
                        .pt(px(tokens.spacing * 2.0))
                        .pb(px(tokens.spacing))
                        .text_size(px(tokens.secondary_font_size))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.muted)
                                .child(SharedString::from(format!(
                                    "{} · {count}",
                                    row.group.title()
                                ))),
                        )
                        .children(row.group.warning().map(|warning| {
                            div()
                                .flex()
                                .gap(px(tokens.spacing))
                                .text_color(theme.danger)
                                .child(crate::icons::glyph(crate::icons::Icon::StateWarning))
                                .child(warning)
                        })),
                );
            }
            let selected = index == sheet.selected;
            let checkable = row.checkable();
            let detail = match row.group {
                Group::NotCaptured => row
                    .entry
                    .reason
                    .clone()
                    .unwrap_or_else(|| "not captured".to_owned()),
                _ => row.action_text().to_owned(),
            };
            let hover = theme.surface_hover;
            list = list.child(
                div()
                    .id(("rewind-file", index))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .px(px(tokens.spacing * 1.5))
                    .h(px(tokens.sidebar_row))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(!selected, |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_rewind_file(index, cx);
                    }))
                    .child(
                        div()
                            .id(("rewind-check", index))
                            .relative()
                            .flex_none()
                            .when(!checkable, |s| s.opacity(0.4))
                            .when(checkable, |s| {
                                s.cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_rewind_file(index, cx);
                                    }))
                            })
                            .child(crate::icons::glyph(if row.checked && checkable {
                                crate::icons::Icon::StateCompleted
                            } else {
                                crate::icons::Icon::StateTodo
                            }))
                            .child(probe(&regions, Region::RewindFile(index))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .font_family(look.mono.clone())
                            .text_color(if checkable { theme.fg } else { theme.faint })
                            .child(SharedString::from(row.entry.path.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(detail)),
                    ),
            );
        }

        // The selected file: `now → restored`.
        let diff = sheet.rows.get(sheet.selected).map(|row| {
            let path = row.entry.path.clone();
            let body: AnyElement = match sheet.diffs.get(&path) {
                Some(DiffState::Ready(Some(file))) => div()
                    .flex()
                    .flex_col()
                    .children(file.lines.iter().map(|line| diff_row(line, &look)))
                    .when(file.lines.is_empty(), |s| {
                        s.child(
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .text_color(theme.muted)
                                .child("No text to show (a binary file or a mode change)."),
                        )
                    })
                    .into_any_element(),
                Some(DiffState::Ready(None)) => div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child("Already as it was: nothing differs.")
                    .into_any_element(),
                Some(DiffState::Failed(error)) => div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child(SharedString::from(format!("No diff: {error}")))
                    .into_any_element(),
                Some(DiffState::Loading) | None if row.group == Group::NotCaptured => div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child("Not captured: there is nothing to restore it from.")
                    .into_any_element(),
                Some(DiffState::Loading) | None => div()
                    .px(px(tokens.spacing * 2.0))
                    .text_color(theme.muted)
                    .child("Reading the diff…")
                    .into_any_element(),
            };
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(format!("{path} · now → restored"))),
                )
                .child(
                    div()
                        .id("rewind-diff")
                        .relative()
                        .max_h(px(DIFF_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .overflow_x_scroll()
                        .rounded(px(tokens.radius))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.bg)
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.font_size - 0.5))
                        .whitespace_nowrap()
                        .py(px(tokens.spacing))
                        .child(body)
                        .child(probe(&regions, Region::Rewind("diff"))),
                )
        });

        let body: AnyElement = match &sheet.phase {
            Phase::Loading => div()
                .text_color(theme.muted)
                .child("Reading the plan…")
                .into_any_element(),
            Phase::Failed(error) => div()
                .text_color(theme.danger)
                .child(SharedString::from(format!(
                    "Cannot preview the restore: {error}"
                )))
                .into_any_element(),
            Phase::Ready if sheet.rows.is_empty() => div()
                .text_color(theme.muted)
                .child("Nothing to restore: every file is as it was before the turn.")
                .into_any_element(),
            Phase::Ready => div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 2.0))
                .child(list)
                .children(diff)
                .into_any_element(),
        };

        let restore_label_text: SharedString = if sheet.working {
            "Restoring…".into()
        } else {
            sheet.restore_label().into()
        };
        let conflicts = sheet.conflicts();
        let all_label: SharedString = if sheet.armed_all {
            format!("Click again to overwrite {conflicts} edited")
        } else {
            "Restore all, including edited-after".to_owned()
        }
        .into();
        let ready = matches!(sheet.phase, Phase::Ready) && !sheet.working && !sheet.busy();
        let can_restore = sheet.can_restore();
        let note = (!sheet.undo).then_some(sheet.note);
        let notice = sheet.notice.clone();
        let error = sheet.error.clone();

        Some(
            div()
                .id("rewind-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(72.0))
                .bg(theme.scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_rewind(window, cx)),
                )
                .child(
                    div()
                        .id("rewind-sheet")
                        .key_context("RewindSheet")
                        .when(wanted, |s| s.track_focus(&self.rewind.focus))
                        .relative()
                        .w(px(680.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.font_size))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                window.focus(&this.rewind.focus, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if this.rewind_key(event, window, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .child(probe(&regions, Region::Rewind("sheet")))
                        .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
                        .children(header.as_ref().map(|header| {
                            muted_with_path(
                                "rewind-facts",
                                &header.facts,
                                &header.checkout,
                                &theme,
                                &tokens,
                            )
                        }))
                        .children(shared.map(|line| {
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(line))
                        }))
                        .children(header.as_ref().map(|header| {
                            muted_with_path(
                                "rewind-label",
                                &header.label,
                                &header.checkout,
                                &theme,
                                &tokens,
                            )
                        }))
                        .children(notice.map(|notice| {
                            div()
                                .relative()
                                .flex()
                                .gap(px(tokens.spacing))
                                .text_color(theme.warning)
                                .child(crate::icons::glyph(crate::icons::Icon::StateWarning))
                                .child(SharedString::from(notice))
                                .child(probe(&regions, Region::Rewind("notice")))
                        }))
                        .children(busy.map(|line| {
                            div()
                                .relative()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(tokens.spacing * 2.0))
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(tokens.spacing))
                                        .text_color(theme.warning)
                                        .child(crate::icons::glyph(crate::icons::Icon::StateWarning))
                                        .child(SharedString::from(line)),
                                )
                                .child(
                                    button(
                                        "rewind-interrupt",
                                        "Interrupt it".into(),
                                        ui::ButtonKind::Secondary,
                                        true,
                                        SheetFocus::Interrupt,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.press_rewind(SheetFocus::Interrupt, window, cx);
                                    }))
                                    .child(probe(&regions, Region::Rewind("interrupt"))),
                                )
                                .child(probe(&regions, Region::Rewind("busy")))
                        }))
                        .children(error.map(|error| {
                            div()
                                .flex()
                                .gap(px(tokens.spacing))
                                .text_color(theme.danger)
                                .child(crate::icons::glyph(crate::icons::Icon::StateFailed))
                                .child(SharedString::from(error))
                        }))
                        .child(body)
                        .children(note.map(|on| {
                            div()
                                .id("rewind-note")
                                .relative()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing * 2.0))
                                .cursor_pointer()
                                .text_size(px(tokens.secondary_font_size))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_rewind_note(cx)))
                                .child(crate::icons::glyph(if on {
                                    crate::icons::Icon::StateCompleted
                                } else {
                                    crate::icons::Icon::StateTodo
                                }))
                                .child(SharedString::from(format!(
                                    "Put a note for {harness} in the composer: which files were restored, and to disregard that turn"
                                )))
                                .child(probe(&regions, Region::Rewind("note")))
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .child(
                                    button(
                                        "rewind-cancel",
                                        "Cancel".into(),
                                        ui::ButtonKind::Secondary,
                                        true,
                                        SheetFocus::Cancel,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.press_rewind(SheetFocus::Cancel, window, cx);
                                    }))
                                    .child(probe(&regions, Region::Rewind("cancel"))),
                                )
                                .when(conflicts > 0, |s| {
                                    s.child(
                                        button(
                                            "rewind-all",
                                            all_label,
                                            ui::ButtonKind::Secondary,
                                            ready,
                                            SheetFocus::RestoreAll,
                                        )
                                        .when(ready, |b| {
                                            b.on_click(cx.listener(|this, _, window, cx| {
                                                this.press_rewind(SheetFocus::RestoreAll, window, cx);
                                            }))
                                        })
                                        .child(probe(&regions, Region::Rewind("restore-all"))),
                                    )
                                })
                                .child(
                                    button(
                                        "rewind-restore",
                                        restore_label_text,
                                        ui::ButtonKind::Primary,
                                        can_restore,
                                        SheetFocus::Restore,
                                    )
                                    .when(can_restore, |b| {
                                        b.on_click(cx.listener(|this, _, window, cx| {
                                            this.press_rewind(SheetFocus::Restore, window, cx);
                                        }))
                                    })
                                    .child(probe(&regions, Region::Rewind("restore"))),
                                ),
                        )
                        .presented(
                            "rewind-sheet-motion",
                            motion::Kind::Sheet,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .presented("rewind-scrim", motion::Kind::Sheet, wanted, motion::Slide::None)
                .inert(!wanted)
.into_any_element(),
        )
    }

    pub(crate) fn render_checkpoints_list(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.rewind.list.is_some() {
            self.exits.checkpoints.cancel();
            return self.build_checkpoints_list(true, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.rewind.list,
            |this| &mut this.exits.checkpoints,
            |this| this.build_checkpoints_list(false, cx),
        )
        .flatten()
    }

    /// `wanted` is false while the list leaves: it takes no input.
    fn build_checkpoints_list(
        &mut self,
        wanted: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let list = self.rewind.list.as_ref()?;
        let session = list.session;
        let selected = list.selected;
        let theme = self.theme;
        let tokens = self.density.ui();
        let look = Look::of(cx);
        let regions = self.regions.clone();
        let rows = self.checkpoint_rows(cx);
        let store = self.sessions.read(cx).checkpoints().clone();
        let (loaded, error, can_restore) = {
            let store = store.read(cx);
            (
                store.loaded(session),
                store.error(session).map(str::to_owned),
                store.restore_supported(),
            )
        };
        let title = self
            .sessions
            .read(cx)
            .session(session)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let hover = theme.surface_hover;
        let body: AnyElement = if let Some(error) = error {
            div()
                .text_color(theme.danger)
                .child(SharedString::from(format!(
                    "Cannot list the checkpoints: {error}"
                )))
                .into_any_element()
        } else if !loaded && rows.is_empty() {
            div()
                .text_color(theme.muted)
                .child("Reading the checkpoints…")
                .into_any_element()
        } else if rows.is_empty() {
            div()
                .text_color(theme.muted)
                .child("No checkpoint keeps files yet: a turn that changes files gets one.")
                .into_any_element()
        } else {
            div()
                .id("checkpoints-rows")
                .flex()
                .flex_col()
                .max_h(px(420.0))
                .overflow_y_scroll()
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    let c = &row.checkpoint;
                    let what = match (c.kind, &row.prompt) {
                        (CheckpointKind::Restore, _) => "A restore of files".to_owned(),
                        (CheckpointKind::Turn, Some(prompt)) => prompt.clone(),
                        (CheckpointKind::Turn, None) => "An earlier turn".to_owned(),
                    };
                    let mut detail = vec![changed_text(c)];
                    if !c.skipped.is_empty() {
                        detail.push(format!("{} not captured", c.skipped.len()));
                    }
                    let state = match c.state {
                        CheckpointState::Ok => None,
                        CheckpointState::Late => Some("recorded late: cannot be rewound"),
                        CheckpointState::Failed => Some("failed"),
                        CheckpointState::Off => Some("checkpoints were off"),
                        _ => Some("not restorable"),
                    };
                    let rewindable = c.state == CheckpointState::Ok && can_restore;
                    div()
                        .id(("checkpoint-row", index))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .px(px(tokens.spacing * 2.0))
                        .py(px(tokens.spacing))
                        .rounded(px(tokens.radius))
                        .when(index == selected, |s| s.bg(theme.accent_soft))
                        .when(index != selected, |s| s.hover(move |s| s.bg(hover)))
                        .child(
                            div()
                                .flex_none()
                                .w(px(88.0))
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(hutan_client::time::relative_to_now(
                                    c.created_ms,
                                ))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(SharedString::from(what)))
                                .child(
                                    div()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.muted)
                                        .font_family(look.mono.clone())
                                        .child(SharedString::from(detail.join(" · "))),
                                )
                                .children(state.map(|state| {
                                    div()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.warning)
                                        .child(state)
                                })),
                        )
                        .child(
                            Button::new(("checkpoint-rewind", index), "Rewind…", &theme, &tokens)
                                .disabled(!rewindable)
                                .when(rewindable, |b| {
                                    b.on_click(cx.listener(move |this, _, window, cx| {
                                        this.rewind_list_row(index, window, cx);
                                    }))
                                })
                                .child(probe(&regions, Region::CheckpointRewind(index))),
                        )
                        .child(probe(&regions, Region::CheckpointRow(index)))
                }))
                .into_any_element()
        };
        Some(
            div()
                .id("checkpoints-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(72.0))
                .bg(theme.scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_checkpoints(window, cx)),
                )
                .child(
                    div()
                        .id("checkpoints-sheet")
                        .key_context("CheckpointsList")
                        .when(wanted, |s| s.track_focus(&self.rewind.list_focus))
                        .relative()
                        .w(px(620.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 4.0))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                window.focus(&this.rewind.list_focus, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if this.rewind.sheet.is_none() && this.checkpoints_key(event, window, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .child(probe(&regions, Region::Checkpoints("sheet")))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(SharedString::from(if title.is_empty() {
                                            "Checkpoints".to_owned()
                                        } else {
                                            format!("Checkpoints — {title}")
                                        })),
                                )
                                .child(
                                    Button::new("checkpoints-close", "Close", &theme, &tokens).ghost()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_checkpoints(window, cx);
                                    }))
                                    .child(probe(&regions, Region::Checkpoints("close"))),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(
                                    "Each turn that changed files can be rewound: its files go back to \
                                     before it, after a preview. Newest first.",
                                ),
                        )
                        .child(body)
                        .presented(
                            "checkpoints-motion",
                            motion::Kind::Sheet,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .presented("checkpoints-scrim", motion::Kind::Sheet, wanted, motion::Slide::None)
                .inert(!wanted)
.into_any_element(),
        )
    }

    pub(crate) fn render_restore_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.restore_toast_exit
                .follow(self.rewind.toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "restore-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("restore-toast")
                    .absolute()
                    .bottom(px(tokens.status_bar + tokens.spacing * 3.0))
                    .left(px(tokens.spacing * 4.0))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .popover(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(SharedString::from(toast.text()))
                    .child(probe(&self.regions, Region::RestoreToast("toast")))
                    .child(
                        Button::new("restore-undo", "Undo", &theme, &tokens)
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.undo_restore_toast(window, cx)
                            }))
                            .child(probe(&self.regions, Region::RestoreToast("undo"))),
                    ),
            )
            .into_any_element(),
        )
    }
}
