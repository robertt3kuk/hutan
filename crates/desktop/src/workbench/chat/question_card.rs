//! The card of a question the agent asked (protocol 0.24): a form built from the question's
//! fields, Submit (⌘↩), Decline and Cancel. The card takes the keyboard once a field is clicked
//! (or Tab is pressed with the composer empty): then typing goes to its field and the digits
//! 1–4, which answer approvals, never answer anything.

use gpui::{
    AnyElement, Context, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_chat_view::{Question, QuestionState};
use hutan_core::question::{FieldKind, QuestionField};
use hutan_core::{QuestionId, QuestionResponse};

use super::input::TextInput;
use super::markdown_view::{Ids, Look, blocks};
use super::question::{Input, QuestionForm};
use super::view::ChatView;
use super::widgets::approval_outcome;
use crate::workbench::render::probe;
use crate::workbench::text_field::{EditableText, FieldMut, FieldRef, FieldState, edit_keys};
use crate::workbench::ui::{Button, FocusRingExt as _, StyledExt as _};
use crate::workbench::view::Region;

/// How many options one field offers as buttons before the rest are cut (a form with a hundred
/// options is not a form a card can hold).
const OPTION_LIMIT: usize = 20;

fn field_region(field: usize) -> Region {
    Region::ChatQuestion(8 + field)
}

fn option_region(field: usize, option: usize) -> Region {
    Region::ChatQuestion(100 + field * OPTION_LIMIT + option.min(OPTION_LIMIT - 1))
}

impl ChatView {
    /// Makes sure the pending questions have a form and the settled ones do not.
    pub(super) fn sync_questions(&mut self, cx: &gpui::App) {
        let model = self.model.read(cx);
        let transcript = model.transcript();
        for question in transcript.pending_questions() {
            self.forms
                .entry(question.id)
                .or_insert_with(|| QuestionForm::new(&question.spec));
        }
        // A form lives while its question waits. One whose question ended unanswered (the turn
        // ended, the agent went away) stays as long as it holds something the user typed, so
        // that it can be copied; secret fields are emptied the moment it ends.
        self.forms
            .retain(|id, form| match transcript.question(*id) {
                Some(question) => match question.state {
                    QuestionState::Pending => true,
                    QuestionState::Expired => {
                        form.scrub_secrets(&question.spec);
                        form.sending = false;
                        form.focus = None;
                        !form.typed(&question.spec).is_empty()
                    }
                    _ => false,
                },
                None => false,
            });
        if self.question_focus.is_some_and(|id| {
            transcript
                .question(id)
                .is_none_or(|q| q.state != QuestionState::Pending)
        }) {
            self.question_focus = None;
        }
    }

    /// What the user had typed into the form of question `id` when it ended unanswered, one
    /// `label: text` line per field — what the card's *Copy* puts on the clipboard. `None` while
    /// the question waits, was answered, or had nothing typed.
    pub fn ended_answers(&self, id: QuestionId, cx: &gpui::App) -> Option<String> {
        let question = self.model.read(cx).transcript().question(id)?;
        if question.state != QuestionState::Expired {
            return None;
        }
        let typed = self.forms.get(&id)?.typed(&question.spec);
        (!typed.is_empty()).then(|| {
            typed
                .into_iter()
                .map(|(label, text)| format!("{label}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// Why the last submit of `id` did not go through, as the card shows it.
    pub fn question_error(&self, id: QuestionId) -> Option<String> {
        self.forms.get(&id)?.error.clone()
    }

    /// The oldest question that waits.
    fn first_pending_question(&self, cx: &gpui::App) -> Option<QuestionId> {
        self.model
            .read(cx)
            .transcript()
            .pending_questions()
            .first()
            .map(|q| q.id)
    }

    /// Gives the keyboard to the first field of the oldest pending question.
    pub(super) fn focus_first_question(&mut self, cx: &mut Context<Self>) -> bool {
        self.sync_questions(cx);
        let Some(id) = self.first_pending_question(cx) else {
            return false;
        };
        self.focus_question_field(id, Some(0), cx);
        true
    }

    fn focus_question_field(
        &mut self,
        id: QuestionId,
        field: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.sync_questions(cx);
        if let Some(form) = self.forms.get_mut(&id) {
            form.focus = field.filter(|at| *at < form.inputs.len());
            self.question_focus = Some(id);
            cx.notify();
        }
    }

    /// Sends the form of `id`: checked here first, then by the daemon.
    pub fn submit_question(&mut self, id: QuestionId, cx: &mut Context<Self>) {
        let Some(question) = self.model.read(cx).transcript().question(id).cloned() else {
            return;
        };
        let Some(form) = self.forms.get_mut(&id) else {
            return;
        };
        if form.sending || question.state != QuestionState::Pending {
            return;
        }
        match form.values(&question.spec) {
            Ok(values) => {
                form.error = None;
                self.send_question_response(id, QuestionResponse::Submit { values }, cx);
            }
            Err(message) => {
                form.error = Some(message);
                cx.notify();
            }
        }
    }

    pub fn decline_question(&mut self, id: QuestionId, cx: &mut Context<Self>) {
        self.send_question_response(id, QuestionResponse::Decline, cx);
    }

    pub fn cancel_question(&mut self, id: QuestionId, cx: &mut Context<Self>) {
        self.send_question_response(id, QuestionResponse::Cancel, cx);
    }

    fn send_question_response(
        &mut self,
        id: QuestionId,
        response: QuestionResponse,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self.forms.get_mut(&id) {
            if form.sending {
                return;
            }
            form.sending = true;
        }
        let task = self
            .sessions
            .update(cx, |store, cx| store.answer_question(id, response, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    if let Some(form) = this.forms.get_mut(&id) {
                        form.sending = false;
                        form.error = Some(format!("Could not answer: {error}"));
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Keys while a question card has the keyboard. `true` when the key was used.
    pub(super) fn on_question_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.function;
        let Some(id) = self.question_focus else {
            // Tab with an empty composer walks into the form of the oldest question.
            if plain
                && !mods.shift
                && key == "tab"
                && self.model.read(cx).input().is_empty()
                && self.focus_first_question(cx)
            {
                return true;
            }
            return false;
        };
        let send_key = self.send_key();
        self.sync_questions(cx);
        let Some(form) = self.forms.get_mut(&id) else {
            self.question_focus = None;
            return false;
        };
        if mods.platform && key == "enter" {
            self.submit_question(id, cx);
            return true;
        }
        if plain && key == "escape" {
            self.question_focus = None;
            if let Some(form) = self.forms.get_mut(&id) {
                form.focus = None;
            }
            cx.notify();
            return true;
        }
        if key == "tab" && !mods.platform && !mods.control && !mods.alt {
            form.move_focus(!mods.shift);
            if form.focus.is_none() {
                self.question_focus = None;
            }
            cx.notify();
            return true;
        }
        let multiline = form.focus.is_some_and(|at| {
            self.model
                .read(cx)
                .transcript()
                .question(id)
                .and_then(|q| q.spec.fields.get(at))
                .is_some_and(|f| matches!(f.kind, FieldKind::Multiline))
        });
        let Some(form) = self.forms.get_mut(&id) else {
            return false;
        };
        let focus = form.focus;
        // Choices and toggles answer to space and the arrow keys.
        let on_text = form.focused_text().is_some();
        if !on_text {
            match key {
                "space" | "enter" if plain => {
                    form.toggle(focus.unwrap_or(0));
                    cx.notify();
                    return true;
                }
                _ => return true,
            }
        }
        let last = focus.is_none_or(|at| at + 1 >= form.inputs.len());
        let Some(text) = form.focused_text() else {
            return true;
        };
        if key == "enter" && !mods.control && !mods.function {
            // Return (`chat.send_key`): submits the answer, adds a line to a multi-line one, or
            // moves on from a one-line field; a Return that commits an input method's marked
            // text only commits it. Under `enter` the last field's Return submits.
            if text.marked().is_some() {
                text.unmark();
            } else if send_key.return_sends(mods.shift, mods.alt) && (multiline || last) {
                self.submit_question(id, cx);
                return true;
            } else if multiline {
                text.insert("\n");
            } else if !mods.alt {
                form.move_focus(true);
            }
        } else if !edit_keys(
            &mut FieldMut {
                input: text,
                vim: None,
                multiline,
            },
            event,
            cx,
        ) {
            // The rest is editing; typed text is no key press: it reaches the field through
            // the platform's text input.
            return false;
        }
        if let Some(form) = self.forms.get_mut(&id) {
            form.error = None;
            if form.focus.is_none() {
                self.question_focus = None;
            }
        }
        cx.notify();
        true
    }

    /// Runs `f` over the text field `at` of the question `id`: the platform's text input and the
    /// mouse edit the card's fields through this.
    pub(super) fn with_question_field(
        &mut self,
        id: QuestionId,
        at: usize,
        multiline: bool,
        f: &mut dyn FnMut(&mut FieldMut<'_>),
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.forms.get_mut(&id) else {
            return;
        };
        let input = match form.inputs.get_mut(at) {
            Some(Input::Text(text)) => text,
            Some(Input::Choice { other, .. }) => other,
            _ => return,
        };
        let before = input.text().to_owned();
        f(&mut FieldMut {
            input,
            vim: None,
            multiline,
        });
        if input.text() != before {
            form.error = None;
        }
        cx.notify();
    }

    // ---- the card --------------------------------------------------------------------------

    pub(super) fn question_card(
        &self,
        question: &Question,
        look: &Look,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let id = question.id;
        let pending = question.state == QuestionState::Pending;
        let here = self.sessions.read(cx).question_answered_here(id);
        // Answered, declined or ended: one line, the outcome's glyph in its color and the words
        // `muted`, then the answers that may be shown (secret ones never are).
        let status: Option<((crate::icons::Icon, gpui::Hsla), String, Vec<String>)> =
            match &question.state {
                QuestionState::Pending => None,
                QuestionState::Answered { withheld, values } => {
                    let mut line = format!(
                        "{} · {}",
                        question.decision_text(),
                        question.answered_from(here)
                    );
                    if !withheld.is_empty() {
                        line.push_str(" · secret answers are not kept");
                    }
                    let shown: Vec<String> = question
                        .spec
                        .fields
                        .iter()
                        .filter_map(|f| {
                            values.get(&f.id).map(|v| {
                                let text = match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                format!("{}: {text}", f.label)
                            })
                        })
                        .collect();
                    Some((
                        (crate::icons::Icon::StateCompleted, theme.success),
                        line,
                        shown,
                    ))
                }
                QuestionState::Declined | QuestionState::Cancelled => Some((
                    (crate::icons::Icon::StateIdle, theme.muted),
                    format!(
                        "{} · {}",
                        question.decision_text(),
                        question.answered_from(here)
                    ),
                    Vec::new(),
                )),
                QuestionState::Expired => Some((
                    (crate::icons::Icon::StateIdle, theme.muted),
                    "No answer: the turn ended or the agent went away".to_owned(),
                    Vec::new(),
                )),
            };
        let form = self.forms.get(&id).cloned();
        let fields: Vec<AnyElement> = match (&form, pending) {
            (Some(form), true) => question
                .spec
                .fields
                .iter()
                .enumerate()
                .map(|(at, field)| self.question_field(id, at, field, form, look, cx))
                .collect(),
            _ => Vec::new(),
        };
        let prompt = if question.spec.prompt.trim().is_empty() {
            None
        } else {
            let mut ids = Ids::new(format!("question-{id}"));
            Some(blocks(
                &hutan_chat_view::markdown::parse(&question.spec.prompt),
                look,
                &mut ids,
            ))
        };
        let raw = question
            .spec
            .raw_schema
            .as_ref()
            .filter(|_| pending)
            .map(|schema| {
                div()
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(
                        serde_json::to_string_pretty(schema).unwrap_or_default(),
                    ))
            });
        // The answers the user had typed when the question ended before they sent them.
        let ended = self.ended_answers(id, cx);
        let sending = form.as_ref().is_some_and(|f| f.sending);
        let error = form.as_ref().and_then(|f| f.error.clone());
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(question.spec.title.clone())),
                    )
                    .children(question.spec.source.as_ref().map(|source| {
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(source.clone()))
                    })),
            )
            .children(prompt)
            .children(raw)
            .children(fields)
            .children(error.map(|message| {
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(message))
            }))
            .when(pending, |s| {
                s.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .child(
                            Button::new(
                                SharedString::from(format!("question-submit-{id}")),
                                "Submit ⌘↩",
                                &look.theme,
                                &look.tokens,
                            )
                            .primary()
                            .disabled(sending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.submit_question(id, cx);
                            }))
                            .child(probe(&self.regions, Region::ChatQuestion(0))),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!("question-decline-{id}")),
                                "Decline",
                                &look.theme,
                                &look.tokens,
                            )
                            .disabled(sending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.decline_question(id, cx);
                            }))
                            .child(probe(&self.regions, Region::ChatQuestion(1))),
                        )
                        .child(
                            Button::new(
                                SharedString::from(format!("question-cancel-{id}")),
                                "Cancel",
                                &look.theme,
                                &look.tokens,
                            )
                            .disabled(sending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_question(id, cx);
                            }))
                            .child(probe(&self.regions, Region::ChatQuestion(2))),
                        ),
                )
            })
            .children(status.map(|(mark, line, answers)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(approval_outcome(mark, line, look))
                    .children(answers.into_iter().map(|answer| {
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(answer))
                    }))
            }))
            .children(ended.map(|answers| {
                let copied = answers.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing * 1.5))
                    .child(
                        div()
                            .text_color(theme.warning)
                            .child("This question ended — copy your answers"),
                    )
                    .child(
                        div()
                            .font_family(look.mono.clone())
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(answers)),
                    )
                    .child(
                        div().flex().child(
                            Button::new(
                                SharedString::from(format!("question-copy-{id}")),
                                "Copy",
                                &look.theme,
                                &look.tokens,
                            )
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copied.clone(),
                                ));
                            })
                            .child(probe(&self.regions, Region::ChatQuestion(3))),
                        ),
                    )
            }))
            .into_any_element()
    }

    fn question_field(
        &self,
        id: QuestionId,
        at: usize,
        field: &QuestionField,
        form: &QuestionForm,
        look: &Look,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let focused = form.focus == Some(at);
        let label = format!("{}{}", field.label, if field.required { " *" } else { "" });
        let typing = focused && self.question_focus == Some(id);
        let text_box = |input: &TextInput, placeholder: &str, secret: bool, tall: bool| {
            let state = FieldState::of(input, None);
            let field = FieldRef::new(
                cx.weak_entity(),
                self.focus_handle(cx),
                move |this: &mut ChatView, cx, f| this.with_question_field(id, at, tall, f, cx),
            );
            div()
                .id(SharedString::from(format!("question-field-{id}-{at}")))
                .min_h(px(if tall { 72.0 } else { 28.0 }))
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .outlined(theme, tokens)
                .focus_ring(typing, theme, tokens)
                .cursor_text()
                .flex()
                .child(
                    div().flex_1().min_w_0().child(
                        EditableText::new(
                            SharedString::from(format!("question-text-{id}-{at}")),
                            state,
                            theme,
                        )
                        .placeholder(placeholder.to_owned())
                        .multiline(tall)
                        .masked(secret)
                        .focused(typing)
                        .field(Some(field)),
                    ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.focus_question_field(id, Some(at), cx);
                }))
                .child(probe(&self.regions, field_region(at)))
        };
        let control: AnyElement = match (&field.kind, &form.inputs[at]) {
            (FieldKind::Boolean, Input::Boolean(on)) => {
                let on = *on;
                // A picked answer is a selection (an `accent_soft` fill), not a primary button:
                // Submit is the card's one primary.
                crate::workbench::ui::choice(
                    SharedString::from(format!("question-toggle-{id}-{at}")),
                    if on { "Yes" } else { "No" },
                    on,
                    theme,
                    tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this.forms.get_mut(&id) {
                        form.toggle(at);
                    }
                    this.focus_question_field(id, Some(at), cx);
                }))
                .child(probe(&self.regions, field_region(at)))
                .into_any_element()
            }
            (
                FieldKind::Choice {
                    options,
                    multi: _,
                    allow_other,
                },
                Input::Choice { chosen, other },
            ) => {
                let buttons = options
                    .iter()
                    .enumerate()
                    .take(OPTION_LIMIT)
                    .map(|(k, option)| {
                        let value = option.value.clone();
                        let picked = chosen.contains(&option.value);
                        let mark = if picked {
                            crate::icons::Icon::StateCompleted
                        } else {
                            crate::icons::Icon::StateTodo
                        };
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                crate::workbench::ui::choice(
                                    SharedString::from(format!("question-option-{id}-{at}-{k}")),
                                    option.label.clone(),
                                    picked,
                                    theme,
                                    tokens,
                                )
                                .gap(px(tokens.spacing))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let spec_fields = this
                                        .model
                                        .read(cx)
                                        .transcript()
                                        .question(id)
                                        .map(|q| q.spec.fields.clone())
                                        .unwrap_or_default();
                                    if let Some(form) = this.forms.get_mut(&id) {
                                        form.choose(&spec_fields, at, &value);
                                    }
                                    cx.notify();
                                }))
                                .child(crate::icons::glyph(mark))
                                .child(probe(&self.regions, option_region(at, k))),
                            )
                            .children(option.description.as_ref().map(|d| {
                                div()
                                    .text_color(theme.muted)
                                    .text_size(px(tokens.secondary_font_size))
                                    .child(SharedString::from(d.clone()))
                            }))
                    });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(tokens.spacing * 2.0))
                            .children(buttons),
                    )
                    .when(*allow_other, |s| {
                        s.child(text_box(other, "Or type your own answer", false, false))
                    })
                    .into_any_element()
            }
            (kind, Input::Text(text)) => text_box(
                text,
                match kind {
                    FieldKind::Number { .. } => "Number",
                    FieldKind::Secret => "Hidden while you type; never kept",
                    _ => "Type here",
                },
                matches!(kind, FieldKind::Secret),
                matches!(kind, FieldKind::Multiline),
            )
            .into_any_element(),
            _ => div().into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(label)),
            )
            .children(field.description.as_ref().map(|d| {
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(d.clone()))
            }))
            .child(control)
            .into_any_element()
    }
}
