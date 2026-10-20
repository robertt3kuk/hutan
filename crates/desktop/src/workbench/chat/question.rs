//! The form of a question card: what the user has typed or chosen so far, and the values it
//! turns into (protocol 0.24). No GPUI; the view feeds it keystrokes and clicks.
//!
//! A secret field's text lives here and in the request that carries it, nowhere else: the form's
//! `Debug` never prints a field's content.

use std::collections::BTreeSet;

use hutan_core::question::{self, FieldKind, QuestionField, QuestionSpec, Values};
use serde_json::Value;

use super::input::TextInput;

/// What one field holds.
#[derive(Clone)]
pub enum Input {
    /// Text, multi-line text, a number, a secret.
    Text(TextInput),
    Boolean(bool),
    /// The chosen options (by value), and the free answer when the field allows one.
    Choice {
        chosen: BTreeSet<String>,
        other: TextInput,
    },
}

/// The form of one question.
#[derive(Clone)]
pub struct QuestionForm {
    pub inputs: Vec<Input>,
    /// The field that has the keyboard; `None` when the card has not been clicked into.
    pub focus: Option<usize>,
    /// Why the last submit did not go through.
    pub error: Option<String>,
    /// An answer is on its way to the daemon.
    pub sending: bool,
}

impl std::fmt::Debug for QuestionForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionForm")
            .field("fields", &self.inputs.len())
            .field("focus", &self.focus)
            .finish_non_exhaustive()
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

impl QuestionForm {
    /// A form with every field at its default.
    pub fn new(spec: &QuestionSpec) -> Self {
        let inputs = spec
            .fields
            .iter()
            .map(|field| match &field.kind {
                FieldKind::Boolean => Input::Boolean(
                    field
                        .default
                        .as_ref()
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
                FieldKind::Choice { .. } => {
                    let chosen = match &field.default {
                        Some(Value::String(one)) => BTreeSet::from([one.clone()]),
                        Some(Value::Array(many)) => many
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        _ => BTreeSet::new(),
                    };
                    Input::Choice {
                        chosen,
                        other: TextInput::new(),
                    }
                }
                _ => {
                    let mut text = TextInput::new();
                    if let Some(default) = &field.default {
                        text.set(&text_of(default));
                    }
                    Input::Text(text)
                }
            })
            .collect();
        Self {
            inputs,
            focus: None,
            error: None,
            sending: false,
        }
    }

    /// The text input under the keyboard: a text-like field, or a choice's free answer.
    pub fn focused_text(&mut self) -> Option<&mut TextInput> {
        match self.inputs.get_mut(self.focus?)? {
            Input::Text(text) => Some(text),
            Input::Choice { other, .. } => Some(other),
            Input::Boolean(_) => None,
        }
    }

    /// Moves the keyboard to the next field (or the previous one); off the end it leaves the card.
    pub fn move_focus(&mut self, forward: bool) {
        let len = self.inputs.len();
        self.focus = match (self.focus, forward) {
            (None, true) if len > 0 => Some(0),
            (None, false) if len > 0 => Some(len - 1),
            (Some(at), true) if at + 1 < len => Some(at + 1),
            (Some(at), false) if at > 0 => Some(at - 1),
            _ => None,
        };
    }

    /// Chooses (or, for a multi-select, flips) an option of field `at`. A single choice drops
    /// the free answer, as the free answer drops the choice.
    pub fn choose(&mut self, fields: &[QuestionField], at: usize, value: &str) {
        let multi = matches!(
            fields.get(at).map(|f| &f.kind),
            Some(FieldKind::Choice { multi: true, .. })
        );
        if let Some(Input::Choice { chosen, other }) = self.inputs.get_mut(at) {
            if multi {
                if !chosen.remove(value) {
                    chosen.insert(value.to_owned());
                }
            } else {
                chosen.clear();
                chosen.insert(value.to_owned());
                other.clear();
            }
        }
        self.error = None;
    }

    pub fn toggle(&mut self, at: usize) {
        if let Some(Input::Boolean(on)) = self.inputs.get_mut(at) {
            *on = !*on;
        }
        self.error = None;
    }

    /// What the user typed or chose and did not send, as `(label, text)` lines: the fields that
    /// differ from their default and are not empty. A secret field is never listed (and
    /// [`scrub_secrets`](Self::scrub_secrets) empties it).
    pub fn typed(&self, spec: &QuestionSpec) -> Vec<(String, String)> {
        let fresh = Self::new(spec);
        let mut lines = Vec::new();
        for ((field, input), start) in spec.fields.iter().zip(&self.inputs).zip(&fresh.inputs) {
            if matches!(field.kind, FieldKind::Secret) {
                continue;
            }
            let text = match (input, start) {
                (Input::Text(now), Input::Text(was)) if now.text() != was.text() => {
                    now.text().to_owned()
                }
                (Input::Boolean(now), Input::Boolean(was)) if now != was => {
                    if *now { "Yes" } else { "No" }.to_owned()
                }
                (
                    Input::Choice { chosen, other },
                    Input::Choice {
                        chosen: start_chosen,
                        other: start_other,
                    },
                ) if chosen != start_chosen || other.text() != start_other.text() => {
                    let labels = match &field.kind {
                        FieldKind::Choice { options, .. } => chosen
                            .iter()
                            .map(|value| {
                                options
                                    .iter()
                                    .find(|o| o.value == *value)
                                    .map_or(value.clone(), |o| o.label.clone())
                            })
                            .collect::<Vec<_>>(),
                        _ => Vec::new(),
                    };
                    let mut parts = labels;
                    if !other.text().trim().is_empty() {
                        parts.push(other.text().trim().to_owned());
                    }
                    parts.join(", ")
                }
                _ => continue,
            };
            if !text.trim().is_empty() {
                lines.push((field.label.clone(), text));
            }
        }
        lines
    }

    /// Empties every secret field: nothing typed into one outlives the question it was for.
    pub fn scrub_secrets(&mut self, spec: &QuestionSpec) {
        for (field, input) in spec.fields.iter().zip(&mut self.inputs) {
            if matches!(field.kind, FieldKind::Secret)
                && let Input::Text(text) = input
            {
                text.clear();
            }
        }
    }

    /// The values the form holds, checked against the question's fields. An empty optional
    /// field is left out.
    pub fn values(&self, spec: &QuestionSpec) -> Result<Values, String> {
        let mut given = Values::new();
        for (field, input) in spec.fields.iter().zip(&self.inputs) {
            let value = match (&field.kind, input) {
                (FieldKind::Number { integer, .. }, Input::Text(text)) => {
                    let text = text.text().trim();
                    if text.is_empty() {
                        continue;
                    }
                    text.parse::<i64>()
                        .map(Value::from)
                        .ok()
                        .or_else(|| {
                            (!integer)
                                .then(|| text.parse::<f64>().ok())
                                .flatten()
                                .and_then(serde_json::Number::from_f64)
                                .map(Value::Number)
                        })
                        .ok_or_else(|| format!("{} must be a number", field.label))?
                }
                (_, Input::Text(text)) => Value::String(text.text().to_owned()),
                (_, Input::Boolean(on)) => Value::Bool(*on),
                (FieldKind::Choice { multi, .. }, Input::Choice { chosen, other }) => {
                    let free = other.text().trim();
                    if *multi {
                        let mut items: Vec<Value> =
                            chosen.iter().cloned().map(Value::String).collect();
                        if !free.is_empty() {
                            items.push(Value::String(free.to_owned()));
                        }
                        Value::Array(items)
                    } else if !free.is_empty() {
                        Value::String(free.to_owned())
                    } else {
                        match chosen.iter().next() {
                            Some(one) => Value::String(one.clone()),
                            None => continue,
                        }
                    }
                }
                _ => continue,
            };
            given.insert(field.id.clone(), value);
        }
        // The form always sends what it shows: a default that was cleared stays cleared.
        let mut fields = spec.fields.clone();
        for field in &mut fields {
            field.default = None;
        }
        question::validate(&fields, &given).map_err(|e| match e {
            question::AnswerError::Missing(id) => {
                let label = spec
                    .fields
                    .iter()
                    .find(|f| f.id == id)
                    .map_or(id, |f| f.label.clone());
                format!("{label} is required")
            }
            other => other.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::question::{ChoiceOption, QuestionExpiry};
    use serde_json::json;

    use super::*;

    fn field(id: &str, kind: FieldKind, required: bool) -> QuestionField {
        QuestionField {
            id: id.into(),
            label: id.to_uppercase(),
            description: None,
            kind,
            required,
            default: None,
        }
    }

    fn spec(fields: Vec<QuestionField>) -> QuestionSpec {
        QuestionSpec {
            title: "t".into(),
            prompt: String::new(),
            fields,
            raw_schema: None,
            source: None,
            expires: QuestionExpiry::Turn,
        }
    }

    fn options(values: &[&str]) -> Vec<ChoiceOption> {
        values
            .iter()
            .map(|v| ChoiceOption {
                value: (*v).into(),
                label: (*v).into(),
                description: None,
            })
            .collect()
    }

    #[test]
    fn defaults_fill_the_form_and_required_fields_must_be_answered() {
        let mut name = field("name", FieldKind::Text, true);
        name.default = Some(json!("anon"));
        let mut flag = field("flag", FieldKind::Boolean, false);
        flag.default = Some(json!(true));
        let spec = spec(vec![name, flag, field("why", FieldKind::Text, true)]);
        let mut form = QuestionForm::new(&spec);
        assert_eq!(form.values(&spec), Err("WHY is required".to_owned()));
        form.focus = Some(2);
        form.focused_text().unwrap().insert("because");
        let values = form.values(&spec).unwrap();
        assert_eq!(values["name"], "anon");
        assert_eq!(values["flag"], true);
        assert_eq!(values["why"], "because");
    }

    #[test]
    fn choices_single_multi_and_free_answers() {
        let one = field(
            "one",
            FieldKind::Choice {
                options: options(&["a", "b"]),
                multi: false,
                allow_other: true,
            },
            true,
        );
        let many = field(
            "many",
            FieldKind::Choice {
                options: options(&["x", "y", "z"]),
                multi: true,
                allow_other: false,
            },
            false,
        );
        let spec = spec(vec![one, many]);
        let mut form = QuestionForm::new(&spec);
        form.choose(&spec.fields, 0, "a");
        form.choose(&spec.fields, 0, "b");
        form.choose(&spec.fields, 1, "x");
        form.choose(&spec.fields, 1, "z");
        form.choose(&spec.fields, 1, "x");
        let values = form.values(&spec).unwrap();
        assert_eq!(
            values["one"], "b",
            "a single choice replaces the one before"
        );
        assert_eq!(values["many"], json!(["z"]), "a multi-select flips");
        // A free answer replaces the choice.
        form.focus = Some(0);
        form.focused_text().unwrap().insert("c");
        assert_eq!(form.values(&spec).unwrap()["one"], "c");
    }

    #[test]
    fn numbers_are_parsed_and_wrong_ones_refused() {
        let spec = spec(vec![field(
            "n",
            FieldKind::Number {
                integer: true,
                min: Some(1.0),
                max: None,
            },
            false,
        )]);
        let mut form = QuestionForm::new(&spec);
        assert!(form.values(&spec).unwrap().is_empty(), "optional and empty");
        form.focus = Some(0);
        form.focused_text().unwrap().insert("7");
        assert_eq!(form.values(&spec).unwrap()["n"], 7);
        form.focused_text().unwrap().insert("x");
        assert_eq!(form.values(&spec), Err("N must be a number".to_owned()));
    }

    #[test]
    fn what_was_typed_is_listed_without_secrets_defaults_or_empty_fields() {
        let mut named = field("name", FieldKind::Text, false);
        named.default = Some(json!("anon"));
        let pick = field(
            "pick",
            FieldKind::Choice {
                options: options(&["a", "b"]),
                multi: true,
                allow_other: true,
            },
            false,
        );
        let spec = spec(vec![
            named,
            field("why", FieldKind::Text, false),
            field("flag", FieldKind::Boolean, false),
            field("key", FieldKind::Secret, false),
            pick,
        ]);
        let mut form = QuestionForm::new(&spec);
        assert!(
            form.typed(&spec).is_empty(),
            "nothing touched, nothing to keep"
        );
        form.focus = Some(1);
        form.focused_text().unwrap().insert("because");
        form.toggle(2);
        form.focus = Some(3);
        form.focused_text().unwrap().insert("hunter2");
        form.choose(&spec.fields, 4, "b");
        form.focus = Some(4);
        form.focused_text().unwrap().insert("or this");
        assert_eq!(
            form.typed(&spec),
            [
                ("WHY".to_owned(), "because".to_owned()),
                ("FLAG".to_owned(), "Yes".to_owned()),
                ("PICK".to_owned(), "b, or this".to_owned()),
            ]
        );
        form.scrub_secrets(&spec);
        form.focus = Some(3);
        assert!(form.focused_text().unwrap().is_empty());
    }

    #[test]
    fn focus_walks_the_fields_and_leaves_at_the_ends_and_debug_hides_content() {
        let spec = spec(vec![
            field("a", FieldKind::Text, false),
            field("k", FieldKind::Secret, false),
        ]);
        let mut form = QuestionForm::new(&spec);
        form.move_focus(true);
        assert_eq!(form.focus, Some(0));
        form.move_focus(true);
        form.focused_text().unwrap().insert("hunter2");
        assert!(!format!("{form:?}").contains("hunter2"));
        form.move_focus(true);
        assert_eq!(form.focus, None);
        form.move_focus(false);
        assert_eq!(form.focus, Some(1));
    }
}
