//! A small confirm / prompt dialog (stop session, rename session, save layout as preset, quit
//! with live sessions).

use crate::theme::DensityExt as _;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};

use super::actions::modal as modal_actions;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{self, FocusRingExt as _, StyledExt as _};
use super::view::Workbench;
use super::vim_ui::OverlayField;

/// What the dialog hands its action: the prompt text and the state of the checkbox.
#[derive(Clone, Default)]
pub(crate) struct ModalResult {
    pub input: Option<String>,
    pub checked: bool,
}

impl std::fmt::Debug for ModalResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalResult")
            .field("input", &self.input.as_ref().map(|_| "<redacted>"))
            .field("checked", &self.checked)
            .finish()
    }
}

pub(crate) type ModalAction =
    Rc<dyn Fn(&mut Workbench, ModalResult, &mut Window, &mut Context<Workbench>)>;

/// What a prompt shows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PromptSpec<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub initial: &'a str,
    pub confirm_label: &'a str,
}

/// A second choice next to the confirm button (for example "Stop sessions & daemon").
#[derive(Clone)]
pub struct ModalChoice {
    pub label: SharedString,
    pub(crate) run: ModalAction,
}

impl std::fmt::Debug for ModalChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalChoice")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// A "Don't ask again" style checkbox.
#[derive(Debug, Clone)]
pub struct ModalCheckbox {
    pub label: SharedString,
    pub checked: bool,
}

/// What the dialog asks and what happens on OK. `input: Some(..)` makes it a prompt.
/// Cancel (button, Escape, click outside) closes it without running anything.
#[derive(Clone)]
pub struct Modal {
    pub title: SharedString,
    pub message: SharedString,
    pub confirm_label: SharedString,
    pub input: Option<String>,
    /// Write-only input: mask render snapshots and Debug; never put a key in view text.
    pub secure_input: bool,
    pub(crate) choice: Option<ModalChoice>,
    pub checkbox: Option<ModalCheckbox>,
    pub(crate) on_confirm: ModalAction,
}

impl Modal {
    /// The label of the second choice, if the dialog has one.
    pub fn choice_label(&self) -> Option<&str> {
        self.choice.as_ref().map(|choice| choice.label.as_ref())
    }
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modal")
            .field("title", &self.title)
            .field(
                "input",
                &if self.secure_input {
                    Some("<redacted>")
                } else {
                    self.input.as_deref()
                },
            )
            .field("choice", &self.choice.as_ref().map(|c| &c.label))
            .field("checkbox", &self.checkbox)
            .finish_non_exhaustive()
    }
}

impl Workbench {
    pub(crate) fn open_confirm(
        &mut self,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        on_confirm: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Modal {
                title: title.into(),
                message: message.into(),
                confirm_label: confirm_label.into(),
                input: None,
                secure_input: false,
                choice: None,
                checkbox: None,
                on_confirm: Rc::new(move |this, _, window, cx| on_confirm(this, window, cx)),
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_prompt(
        &mut self,
        prompt: PromptSpec<'_>,
        on_confirm: impl Fn(&mut Workbench, String, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Modal {
                title: prompt.title.into(),
                message: prompt.message.into(),
                confirm_label: prompt.confirm_label.into(),
                input: Some(prompt.initial.to_owned()),
                secure_input: false,
                choice: None,
                checkbox: None,
                on_confirm: Rc::new(move |this, result, window, cx| {
                    on_confirm(this, result.input.unwrap_or_default(), window, cx);
                }),
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_none() {
            self.modal_return = self.focus_to_restore(window, cx);
        }
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.modal = Some(modal);
        self.edits.modal.reset();
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    /// What had the keyboard before a dialog opens: the settings sheet while it is open (its
    /// search and controls live under one handle), else the focused element, unless that is
    /// an overlay the dialog itself closes.
    fn focus_to_restore(&self, window: &Window, cx: &gpui::App) -> Option<gpui::WeakFocusHandle> {
        if self.sheet.is_some() {
            return Some(self.sheet_focus.downgrade());
        }
        let focused = window.focused(cx)?;
        let overlay = [
            &self.modal_focus,
            &self.palette_focus,
            &self.picker_focus,
            &self.menu_focus,
        ]
        .contains(&&focused);
        (!overlay).then(|| focused.downgrade())
    }

    /// Gives the keyboard back to what had it before the dialog opened, if that is still
    /// there; else to the settings sheet while it is open, else to the focused pane.
    fn restore_modal_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self
            .modal_return
            .take()
            .and_then(|weak| weak.upgrade())
            .filter(|handle| *handle != self.modal_focus);
        if let Some(handle) = previous {
            window.focus(&handle, cx);
        } else if self.sheet.is_some() {
            window.focus(&self.sheet_focus, cx);
        } else {
            self.focus_root(window, cx);
        }
    }

    /// The open dialog, if any.
    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    pub fn modal_open(&self) -> bool {
        self.modal.is_some()
    }

    /// Replaces the prompt text.
    pub fn set_modal_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(Modal {
            input: Some(input), ..
        }) = &mut self.modal
        {
            *input = text.to_owned();
            cx.notify();
        }
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.take().is_some() {
            self.restore_modal_focus(window, cx);
            cx.notify();
        }
    }

    /// Ticks or unticks the dialog's checkbox, if it has one.
    pub fn set_modal_checkbox(&mut self, checked: bool, cx: &mut Context<Self>) {
        if let Some(Modal {
            checkbox: Some(checkbox),
            ..
        }) = &mut self.modal
        {
            checkbox.checked = checked;
            cx.notify();
        }
    }

    /// Selects the whole prefilled text of the prompt: the next typed text or paste replaces it.
    pub fn select_modal_input(&mut self, cx: &mut Context<Self>) {
        if let Some(Modal {
            input: Some(input),
            secure_input: false,
            ..
        }) = &self.modal
            && !input.is_empty()
        {
            self.edits.modal.select_all(input);
            cx.notify();
        }
    }

    /// Whether the prompt's text is selected.
    pub fn modal_input_selected(&self) -> bool {
        self.modal
            .as_ref()
            .and_then(|m| m.input.as_deref())
            .is_some_and(|input| self.edits.modal.selected(input))
    }

    fn toggle_modal_checkbox(&mut self, cx: &mut Context<Self>) {
        let checked = self
            .modal
            .as_ref()
            .and_then(|m| m.checkbox.as_ref())
            .is_some_and(|c| c.checked);
        self.set_modal_checkbox(!checked, cx);
    }

    /// Closes the dialog and runs its confirm action (the primary button).
    pub fn confirm_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_modal(false, window, cx);
    }

    /// Closes the dialog and runs its second choice, if it has one.
    pub fn choose_modal_alternative(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_modal(true, window, cx);
    }

    fn finish_modal(&mut self, alternative: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        if modal
            .input
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        {
            // Nothing to submit; keep the dialog open.
            self.modal = Some(modal);
            return;
        }
        if alternative && modal.choice.is_none() {
            self.modal = Some(modal);
            return;
        }
        let action = match &modal.choice {
            Some(choice) if alternative => Rc::clone(&choice.run),
            _ => Rc::clone(&modal.on_confirm),
        };
        let result = ModalResult {
            input: modal.input,
            checked: modal.checkbox.is_some_and(|c| c.checked),
        };
        self.restore_modal_focus(window, cx);
        action(self, result, window, cx);
        cx.notify();
    }

    pub(crate) fn render_modal(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let stored = self.modal.as_ref()?;
        // The prompt's text is not in this snapshot: its field element draws it (a secret as
        // dots) and never keeps it.
        let modal = Modal {
            title: stored.title.clone(),
            message: stored.message.clone(),
            confirm_label: stored.confirm_label.clone(),
            input: stored.input.as_ref().map(|_| String::new()),
            secure_input: stored.secure_input,
            choice: stored.choice.clone(),
            checkbox: stored.checkbox.clone(),
            on_confirm: Rc::clone(&stored.on_confirm),
        };
        let input_parts = modal
            .input
            .is_some()
            .then(|| self.overlay_parts(OverlayField::Modal, "", cx));
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = |id: &'static str, label: SharedString, kind: ui::ButtonKind| {
            ui::button(id, label, kind, true, &theme, &tokens)
        };
        Some(
            div()
                .id("modal-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(120.0))
                .bg(theme.scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                )
                .child(
                    div()
                        .id("modal")
                        .key_context(super::vim_ui::vim_key_context(
                            "Modal",
                            self.overlay_vim_mode(super::vim_ui::OverlayField::Modal),
                        ))
                        .track_focus(&self.modal_focus)
                        .w(px(440.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.font_size))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                window.focus(&this.modal_focus, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            // Typed text arrives through the field's platform text input.
                            if this.overlay_key(OverlayField::Modal, event, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &modal_actions::Confirm, window, cx| {
                            this.confirm_modal(window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &modal_actions::Cancel, window, cx| {
                            if this.modal_vim_escape(cx) {
                                return;
                            }
                            this.close_modal(window, cx);
                        }))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(modal.title.clone()),
                        )
                        .child(div().text_color(theme.muted).child(modal.message.clone()))
                        .when_some(input_parts, move |s, parts| {
                            s.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(tokens.sidebar_row + 4.0))
                                    .px(px(tokens.spacing * 2.0))
                                    .outlined(&theme, &tokens)
                                    .focus_ring(true, &theme, &tokens)
                                    .children(parts),
                            )
                        })
                        .when_some(modal.checkbox.clone(), |s, checkbox| {
                            s.child(
                                div()
                                    .id("modal-checkbox")
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens.spacing * 2.0))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_modal_checkbox(cx);
                                    }))
                                    .child(crate::icons::glyph(if checkbox.checked {
                                        crate::icons::Icon::StateCompleted
                                    } else {
                                        crate::icons::Icon::StateTodo
                                    }))
                                    .child(checkbox.label),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .child(
                                    button("modal-cancel", "Cancel".into(), ui::ButtonKind::Ghost)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_modal(window, cx);
                                        })),
                                )
                                .children(modal.choice.as_ref().map(|choice| {
                                    button(
                                        "modal-choice",
                                        choice.label.clone(),
                                        ui::ButtonKind::Secondary,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.choose_modal_alternative(window, cx);
                                        },
                                    ))
                                }))
                                .child(
                                    button(
                                        "modal-ok",
                                        modal.confirm_label.clone(),
                                        ui::ButtonKind::Primary,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.confirm_modal(window, cx);
                                        },
                                    )),
                                ),
                        )
                        .presented(
                            "modal-motion",
                            motion::Kind::Sheet,
                            true,
                            motion::Slide::Rise,
                        ),
                )
                .presented(
                    "modal-scrim",
                    motion::Kind::Sheet,
                    true,
                    motion::Slide::None,
                )
                .into_any_element(),
        )
    }
}
