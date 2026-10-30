//! Suggested answers (protocol 0.64, capability `ask.suggest`; ADR 0025 R2): the one advisory
//! answer a pending approval or question may carry, from a Mawas chat.
//!
//! Nothing here answers anything. `approval.respond` and `question.answer` stay user-only;
//! the card shows the suggestion as Mawas's words, and the user's click on *Use* is their own
//! answer through those requests. A suggestion lives in memory with the request it is about
//! and goes with it (answered, expired, daemon restart).

use hutan_protocol::suggest::{
    AskSuggest, AskSuggested, AskSuggestion, AskTarget, SUGGEST_ANSWER_MAX_BYTES,
    SUGGEST_REASON_MAX_CHARS, SuggestedAnswer, Suggestion,
};

use super::*;

impl ChatManager {
    /// The suggestion a pending request carries now.
    pub fn suggestion_of(&self, target: AskTarget) -> Option<Suggestion> {
        lock(&self.suggestions).get(&target).cloned()
    }

    /// `ask.suggest` from the Mawas chat `from`: attaches one advisory answer to a pending
    /// approval or question, replacing the one it had.
    pub fn suggest(&self, from: SessionId, request: AskSuggest) -> Result<AskSuggested, ErrorBody> {
        let AskSuggest {
            target,
            answer,
            reason,
        } = request;
        let bad = |message: String| ErrorBody::new(ErrorCode::BadRequest, message);
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Err(bad(
                "say why in `reason`: a suggestion without one is not shown".into(),
            ));
        }
        let chars = reason.chars().count();
        if chars > SUGGEST_REASON_MAX_CHARS {
            return Err(bad(format!(
                "`reason` has at most {SUGGEST_REASON_MAX_CHARS} characters, not {chars}"
            )));
        }
        let size = serde_json::to_string(&answer).map_or(usize::MAX, |json| json.len());
        if size > SUGGEST_ANSWER_MAX_BYTES {
            return Err(bad(format!(
                "a suggested answer has at most {SUGGEST_ANSWER_MAX_BYTES} bytes, not {size}"
            )));
        }
        self.check_suggestion(&target, &answer)?;
        let suggestion = Suggestion {
            answer,
            reason,
            from,
            suggested_at_ms: registry::now_ms(),
        };
        let replaced = lock(&self.suggestions)
            .insert(target, suggestion.clone())
            .is_some();
        self.emit(&Event::AskSuggestion(Box::new(AskSuggestion {
            target,
            suggestion: Some(suggestion.clone()),
        })));
        Ok(AskSuggested {
            target,
            suggestion,
            replaced,
        })
    }

    /// The request is over (answered, or it expired): its suggestion goes with it.
    pub fn clear_suggestion(&self, target: AskTarget) {
        if lock(&self.suggestions).remove(&target).is_some() {
            self.emit(&Event::AskSuggestion(Box::new(AskSuggestion {
                target,
                suggestion: None,
            })));
        }
    }

    /// What the suggestion is about must be waiting, and the answer one the user could give;
    /// returns the session that asks.
    pub fn check_suggestion(
        &self,
        target: &AskTarget,
        answer: &SuggestedAnswer,
    ) -> Result<SessionId, ErrorBody> {
        let bad = |message: String| ErrorBody::new(ErrorCode::BadRequest, message);
        match (target, answer) {
            (AskTarget::Approval { id }, SuggestedAnswer::Option { option_id }) => {
                let approval = self
                    .approval_list(None)
                    .approvals
                    .into_iter()
                    .find(|a| a.id == *id)
                    .ok_or_else(|| {
                        ErrorBody::new(
                            ErrorCode::NotFound,
                            format!("approval {id} is not pending (answered, or its turn ended)"),
                        )
                    })?;
                if !approval.options.iter().any(|o| o.id == *option_id) {
                    let offered: Vec<&str> =
                        approval.options.iter().map(|o| o.id.as_str()).collect();
                    return Err(bad(format!(
                        "option `{option_id}` is not offered by approval {id}; offered: {}",
                        offered.join(", ")
                    )));
                }
                Ok(approval.session_id)
            }
            (AskTarget::Approval { .. }, _) => Err(bad(
                "an approval takes `{\"kind\": \"option\", \"option_id\": …}`".into(),
            )),
            (AskTarget::Question { id }, answer) => {
                let question = lock(&self.questions)
                    .get(id)
                    .map(|p| p.info.clone())
                    .ok_or_else(|| {
                        ErrorBody::new(
                            ErrorCode::NotFound,
                            format!("question {id} is not pending (answered, or its turn ended)"),
                        )
                    })?;
                if question.spec.fields.iter().any(|f| f.is_secret()) {
                    return Err(bad(
                        "a question with a secret field takes no suggestion: only the user \
                         types a secret"
                            .into(),
                    ));
                }
                match answer {
                    SuggestedAnswer::Values { values } => {
                        hutan_core::question::validate(&question.spec.fields, values).map_err(
                            |e| bad(format!("the question would refuse these values: {e}")),
                        )?;
                    }
                    SuggestedAnswer::Decline => {}
                    SuggestedAnswer::Option { .. } => {
                        return Err(bad(
                            "a question takes `{\"kind\": \"values\", \"values\": …}` or \
                             `{\"kind\": \"decline\"}`"
                                .into(),
                        ));
                    }
                }
                Ok(question.session_id)
            }
        }
    }
}
