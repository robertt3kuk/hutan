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
use super::widgets::{approval_outcome, suggestion_box, suggestion_buttons};
use crate::workbench::render::probe;
use crate::workbench::text_field::{EditableText, FieldMut, FieldRef, FieldState, edit_keys};
use crate::workbench::ui::{Button, FocusRingExt as _, StyledExt as _};
use crate::workbench::view::Region;
use hutan_protocol::suggest::{AskTarget, SuggestedAnswer};

/// Space between the groups of the card, and between a field's label and its control (px).
const GROUP_GAP: f32 = 16.0;
const LABEL_GAP: f32 = 8.0;

/// Whether two texts say the same thing: equal without case, spaces and closing punctuation.
fn same_words(a: &str, b: &str) -> bool {
    let norm = |text: &str| {
        text.trim_end_matches(|c: char| c.is_whitespace() || "?:.*".contains(c))
            .trim_start()
            .to_lowercase()
    };
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

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

    /// What the form of question `id` holds now — what Submit would send — or why it would not
    /// send (`None` while the question has no form).
    pub fn question_values(
        &self,
        id: QuestionId,
        cx: &gpui::App,
    ) -> Option<Result<hutan_core::question::Values, String>> {
        let spec = &self.model.read(cx).transcript().question(id)?.spec;
        Some(self.forms.get(&id)?.values(spec))
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

    /// What Mawas suggests for the question (protocol 0.64): her answer in words — each field's
    /// label and the option's label — her reason, *Use* and *Dismiss*. *Use* fills the fields
    /// (nothing is sent: the user reads, changes and submits), or, for *decline*, is the user's
    /// own click on Decline. `None` without a suggestion or once it was dismissed.
    fn question_suggestion(
        &self,
        question: &Question,
        look: &Look,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let id = question.id;
        let suggestion = self.sessions.read(cx).question_suggestion(id)?.clone();
        let what = match &suggestion.answer {
            SuggestedAnswer::Values { values } => question
                .spec
                .fields
                .iter()
                .filter_map(|field| {
                    let value = values.get(&field.id)?;
                    let shown = |one: &str| match &field.kind {
                        FieldKind::Choice { options, .. } => options
                            .iter()
                            .find(|o| o.value == one)
                            .map_or_else(|| one.to_owned(), |o| o.label.clone()),
                        _ => one.to_owned(),
                    };
                    let text = match value {
                        serde_json::Value::String(one) => shown(one),
                        serde_json::Value::Array(many) => many
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(shown)
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    };
                    // A label that only repeats the question is not said again.
                    let echoed = same_words(&field.label, &question.spec.title)
                        || same_words(&field.label, &question.spec.prompt);
                    Some(if echoed {
                        text
                    } else {
                        format!("{}: {text}", field.label)
                    })
                })
                .collect::<Vec<_>>()
                .join(" · "),
            SuggestedAnswer::Decline => "Decline".to_owned(),
            SuggestedAnswer::Option { .. } => return None,
        };
        let answer = suggestion.answer.clone();
        let (use_button, dismiss) = suggestion_buttons(&format!("question-{id}"), look);
        let buttons = vec![
            use_button
                .on_click(cx.listener(move |this, _, _, cx| match &answer {
                    SuggestedAnswer::Values { values } => {
                        let fields = this
                            .model
                            .read(cx)
                            .transcript()
                            .question(id)
                            .map(|q| q.spec.fields.clone());
                        if let (Some(fields), Some(form)) = (fields, this.forms.get_mut(&id)) {
                            form.fill(&fields, values);
                            cx.notify();
                        }
                    }
                    SuggestedAnswer::Decline => this.decline_question(id, cx),
                    SuggestedAnswer::Option { .. } => {}
                }))
                .child(probe(&self.regions, Region::Suggestion("question-use", 0)))
                .into_any_element(),
            dismiss
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sessions.update(cx, |store, cx| {
                        store.dismiss_suggestion(AskTarget::Question { id }, cx);
                    });
                }))
                .child(probe(
                    &self.regions,
                    Region::Suggestion("question-dismiss", 0),
                ))
                .into_any_element(),
        ];
        Some(
            suggestion_box(&what, &suggestion.reason, buttons, look)
                .child(probe(&self.regions, Region::Suggestion("question-box", 0)))
                .into_any_element(),
        )
    }

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
            (Some(form), true) => {
                let alone = question.spec.fields.len() == 1;
                question
                    .spec
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(at, field)| {
                        let echoed = alone
                            && (same_words(&field.label, &question.spec.title)
                                || same_words(&field.label, &question.spec.prompt));
                        self.question_field(id, at, field, form, echoed, cx)
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        // The question is said once: a prompt or a field label that only repeats the title (or
        // the prompt) is not drawn again.
        let prompt = if question.spec.prompt.trim().is_empty()
            || same_words(&question.spec.prompt, &question.spec.title)
        {
            None
        } else {
            let mut ids = Ids::new(format!("question-{id}"));
            Some(blocks(
                &hutan_chat_view::markdown::parse_breaks(&question.spec.prompt),
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
        let suggestion = if pending {
            self.question_suggestion(question, look, cx)
        } else {
            None
        };
        // 16 px between the groups of the card (the question, each field, the buttons), 8 px
        // between a field's label and its control.
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(GROUP_GAP))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
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
                    .children(prompt),
            )
            .children(raw)
            .children(suggestion)
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
                            .ghost()
                            .disabled(sending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.decline_question(id, cx);
                            }))
                            .child(probe(&self.regions, Region::ChatQuestion(1))),
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
        label_echoes_question: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let look = &Look::of(cx);
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
                let rows = options
                    .iter()
                    .enumerate()
                    .take(OPTION_LIMIT)
                    .map(|(k, option)| {
                        let value = option.value.clone();
                        let picked = chosen.contains(&option.value);
                        crate::workbench::ui::option_row(
                            SharedString::from(format!("question-option-{id}-{at}-{k}")),
                            option.label.clone(),
                            option
                                .description
                                .clone()
                                .filter(|d| !d.trim().is_empty())
                                .map(SharedString::from),
                            picked,
                            theme,
                            tokens,
                        )
                        .relative()
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
                        .child(probe(&self.regions, option_region(at, k)))
                    });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(div().flex().flex_col().gap(px(2.0)).children(rows))
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
            .gap(px(LABEL_GAP))
            .children((!label_echoes_question).then(|| {
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(label))
            }))
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

#[cfg(test)]
mod tests {
    use super::same_words;

    #[test]
    fn a_label_that_repeats_the_question_is_the_same_words() {
        assert!(same_words("Which database?", "which database"));
        assert!(same_words("  Which database? * ", "Which database?"));
        assert!(!same_words("Which database?", "Which cache?"));
        assert!(!same_words("", ""), "nothing is not a repeat");
    }
}
