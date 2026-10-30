//! Suggested answers (protocol 0.64, capability `ask.suggest`; ADR 0025 decision 3, rule R2 of
//! `docs/spikes/2026-10-30-mawas-supervisor.md` § 4.3): `ask.suggest`, the `suggestion` of a
//! pending approval or question, and the `ask.suggestion` event.
//!
//! Answering an approval or a question is the user's (`approval.respond` and `question.answer`
//! stay user-only, Mawas included). What Mawas can do is **suggest**: attach one advisory,
//! labelled, unsent answer to the card of a pending request. The card shows it as Mawas's
//! words with *Use* and *Dismiss*; *Use* is the user's own click on the option (or a filled
//! field the user sends), and nothing is ever applied by the suggestion itself.

use hutan_core::question::Values;
use hutan_core::{ApprovalId, QuestionId, SessionId};
use serde::{Deserialize, Serialize};

/// Longest [`AskSuggest::reason`], in characters.
pub const SUGGEST_REASON_MAX_CHARS: usize = 400;
/// Longest suggested answer, in bytes of its JSON.
pub const SUGGEST_ANSWER_MAX_BYTES: usize = 2 * 1024;

/// What a suggestion is about: a pending approval or a pending question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskTarget {
    Approval { id: ApprovalId },
    Question { id: QuestionId },
}

/// The answer suggested. For an approval, one of the options the request offers; for a
/// question, values for its fields (checked against them) or declining it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestedAnswer {
    /// An approval's option, by its id (`approval.options[].id`).
    Option { option_id: String },
    /// A question's answers by field id, as `question.answer` takes them. A question with a
    /// secret field takes no suggestion.
    Values { values: Values },
    /// Decline the question.
    Decline,
}

/// The suggestion a pending request carries: at most one, the newest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub answer: SuggestedAnswer,
    /// Why, in Mawas's words (at most [`SUGGEST_REASON_MAX_CHARS`] characters).
    pub reason: String,
    /// The Mawas chat that suggested it.
    pub from: SessionId,
    pub suggested_at_ms: u64,
}

/// `ask.suggest`: attaches `answer` and `reason` to a pending approval or question, replacing
/// the suggestion it had; answered with [`AskSuggested`]. Only a Mawas chat may ask
/// (`forbidden` otherwise, `decision = "not_mawas"`), and it is a change like the others of
/// Mawas's gate (Advisor refuses it; in Ask before acting it asks unless granted).
///
/// Errors: `not_found` when the target is not pending (answered, or its turn ended);
/// `bad_request` for an option the approval does not offer, values the question refuses, a
/// secret field, a text over [`SUGGEST_ANSWER_MAX_BYTES`] or a reason over
/// [`SUGGEST_REASON_MAX_CHARS`] or empty; `forbidden` (`decision = "mawas_target"`) for a
/// request of a Mawas session (she does not steer the user's answers to her own requests).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskSuggest {
    pub target: AskTarget,
    pub answer: SuggestedAnswer,
    pub reason: String,
}

/// The answer of `ask.suggest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskSuggested {
    pub target: AskTarget,
    pub suggestion: Suggestion,
    /// An earlier suggestion for the same request was replaced.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaced: bool,
}

/// The `ask.suggestion` event: the suggestion of a pending request changed. `suggestion` is
/// absent when it went away because the request was answered or expired. A client keeps the
/// suggestion of each pending request it shows and applies this; `approval.list` and
/// `question.list` carry the current ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskSuggestion {
    pub target: AskTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
}
