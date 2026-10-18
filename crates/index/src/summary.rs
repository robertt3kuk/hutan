//! The per-session summary behind the session catalog (`history.sessions`): provider title,
//! first prompt, latest turns, message count, model, branch, and the first and last time.
//!
//! A summary is folded line by line while a transcript is read (it lives in the adapter's
//! [`ParseState`](crate::ParseState), so an appended file resumes from it) or record by record
//! for Hutan's own sessions. Texts are redacted before they are kept and bounded: the first
//! prompt to [`FIRST_PROMPT_MAX_CHARS`], each turn to [`TURN_TEXT_MAX_CHARS`] on one line, and
//! at most [`LATEST_TURNS_MAX`] turns are in memory.

use serde::{Deserialize, Serialize};

use crate::redact::redact;
use crate::types::Role;

/// Longest first prompt kept, in characters (a longer one ends in `…`).
pub const FIRST_PROMPT_MAX_CHARS: usize = 2000;
/// Longest turn text kept, in characters (a longer one ends in `…`).
pub const TURN_TEXT_MAX_CHARS: usize = 400;
/// Latest turns kept.
pub const LATEST_TURNS_MAX: usize = 3;
/// Longest provider title kept, in characters.
const TITLE_MAX_CHARS: usize = 200;
/// How much of a message is looked at: everything beyond is never part of a summary.
const HEAD_BYTES: usize = 64 * 1024;

/// One turn of a conversation as the catalog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    /// One line, at most [`TURN_TEXT_MAX_CHARS`] characters, redacted.
    pub text: String,
    pub ts_ms: Option<u64>,
}

/// What the catalog knows of one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    /// A title the user gave (Claude Code `custom-title`).
    pub custom_title: Option<String>,
    /// A title the agent generated (Claude `ai-title`, OMP `title`, Codex `thread_name`).
    pub generated_title: Option<String>,
    pub first_prompt: Option<String>,
    /// Oldest first.
    pub latest: Vec<Turn>,
    /// User and assistant messages (Hutan terminals: commands).
    pub messages: u32,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub created_ms: Option<u64>,
    pub updated_ms: Option<u64>,
    /// The transcript belongs to a sub-agent of another session: not listed on its own.
    pub subagent: bool,
}

impl Summary {
    /// The agent's own title for the session: the one the user gave, else the generated one.
    pub fn provider_title(&self) -> Option<&str> {
        self.custom_title
            .as_deref()
            .or(self.generated_title.as_deref())
    }

    pub fn set_custom_title(&mut self, title: &str) {
        if let Some(title) = clean_title(title) {
            self.custom_title = Some(title);
        }
    }

    pub fn set_generated_title(&mut self, title: &str) {
        if let Some(title) = clean_title(title) {
            self.generated_title = Some(title);
        }
    }

    pub fn set_model(&mut self, model: &str) {
        let model = model.trim();
        if !model.is_empty() {
            self.model = Some(model.chars().take(200).collect());
        }
    }

    pub fn set_branch(&mut self, branch: &str) {
        let branch = branch.trim();
        if !branch.is_empty() {
            self.branch = Some(branch.chars().take(200).collect());
        }
    }

    /// A line of the transcript carried this time (any line: messages, tool calls, bookkeeping).
    pub fn note_time(&mut self, ts_ms: Option<u64>) {
        let Some(ts) = ts_ms else { return };
        self.created_ms = Some(self.created_ms.map_or(ts, |first| first.min(ts)));
        self.updated_ms = Some(self.updated_ms.map_or(ts, |last| last.max(ts)));
    }

    /// One message of the conversation: counted, shown as a turn, and (a user's first) kept as
    /// the first prompt.
    pub fn note_message(&mut self, role: Role, ts_ms: Option<u64>, text: &str) {
        self.messages = self.messages.saturating_add(1);
        self.note_turn(role, ts_ms, text);
    }

    /// Like [`note_message`](Self::note_message) without counting (the caller knows the count).
    pub fn note_turn(&mut self, role: Role, ts_ms: Option<u64>, text: &str) {
        self.note_time(ts_ms);
        let redacted = redact(head(text));
        if role == Role::User && self.first_prompt.is_none() {
            let prompt = clip(redacted.trim(), FIRST_PROMPT_MAX_CHARS);
            if !prompt.is_empty() {
                self.first_prompt = Some(prompt);
            }
        }
        let line = one_line(&redacted, TURN_TEXT_MAX_CHARS);
        if line.is_empty() {
            return;
        }
        let turn = Turn {
            role,
            text: line,
            ts_ms,
        };
        // A record pushed again (Hutan replaces rows by key) is not a second turn.
        if self.latest.last() == Some(&turn) {
            return;
        }
        self.latest.push(turn);
        if self.latest.len() > LATEST_TURNS_MAX {
            self.latest.remove(0);
        }
    }
}

/// The first [`HEAD_BYTES`] of `text`, cut at a character boundary.
fn head(text: &str) -> &str {
    if text.len() <= HEAD_BYTES {
        return text;
    }
    let mut end = HEAD_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(crate) fn clean_title(title: &str) -> Option<String> {
    let line = one_line(title, TITLE_MAX_CHARS);
    (!line.is_empty()).then_some(line)
}

/// `text` cut to `max` characters; a longer text ends in `…` (within the `max`).
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Whitespace runs (line breaks included) become one space, then [`clip`].
fn one_line(text: &str, max: usize) -> String {
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
        // Enough to fill `max` characters: the rest is cut anyway.
        if line.len() > max.saturating_mul(4) {
            break;
        }
    }
    clip(&line, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_are_one_line_bounded_and_redacted() {
        let mut summary = Summary::default();
        summary.note_message(
            Role::User,
            Some(5),
            "first\n\n  line   with key sk-abcdefghijklmnopqrstuvwx",
        );
        assert_eq!(
            summary.latest[0].text, "first line with key [REDACTED]",
            "whitespace collapses and secrets go"
        );
        assert!(
            summary
                .first_prompt
                .as_deref()
                .is_some_and(|p| p.starts_with("first\n\n") && !p.contains("sk-"))
        );

        summary.note_message(Role::Assistant, Some(6), &"word ".repeat(500));
        let long = &summary.latest[1].text;
        assert_eq!(long.chars().count(), TURN_TEXT_MAX_CHARS);
        assert!(long.ends_with('…'));
        // Multi-byte text is cut on a character boundary.
        summary.note_message(Role::Assistant, Some(7), &"é".repeat(5000));
        let accents = &summary.latest[2].text;
        assert_eq!(accents.chars().count(), TURN_TEXT_MAX_CHARS);
    }

    #[test]
    fn first_prompt_is_the_first_user_message_and_is_cut_at_the_limit() {
        let mut summary = Summary::default();
        summary.note_message(Role::Assistant, None, "hello");
        assert_eq!(summary.first_prompt, None);
        summary.note_message(Role::User, None, &"x".repeat(5000));
        let prompt = summary.first_prompt.clone().unwrap_or_default();
        assert_eq!(prompt.chars().count(), FIRST_PROMPT_MAX_CHARS);
        assert!(prompt.ends_with('…'));
        summary.note_message(Role::User, None, "second");
        assert_eq!(summary.first_prompt.as_deref(), Some(prompt.as_str()));
    }

    #[test]
    fn only_the_latest_three_turns_stay_and_a_repeat_is_not_a_turn() {
        let mut summary = Summary::default();
        for n in 0..6 {
            summary.note_message(Role::User, Some(n), &format!("m{n}"));
        }
        let texts: Vec<_> = summary.latest.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["m3", "m4", "m5"]);
        assert_eq!(summary.messages, 6);
        summary.note_turn(Role::User, Some(5), "m5");
        assert_eq!(summary.latest.len(), LATEST_TURNS_MAX);
        assert_eq!((summary.created_ms, summary.updated_ms), (Some(0), Some(5)));
    }

    #[test]
    fn titles_prefer_what_the_user_gave() {
        let mut summary = Summary::default();
        summary.set_generated_title("  Generated\ntitle ");
        assert_eq!(summary.provider_title(), Some("Generated title"));
        summary.set_custom_title("Mine");
        summary.set_generated_title("Another generated");
        assert_eq!(summary.provider_title(), Some("Mine"));
        summary.set_custom_title("   ");
        assert_eq!(summary.provider_title(), Some("Mine"));
    }
}
