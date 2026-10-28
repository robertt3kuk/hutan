//! OSC 7501, the program status a coding agent reports about itself (OMP 18.12 and later):
//!
//! ```text
//! ESC ] 7501 ; state=<working|blocked|done|error|idle> [:kind=<kind>] :app=<name> [:msg=<base64>] ESC \
//! ESC ] 7501 ; state=clear ESC \
//! ```
//!
//! `kind` and `msg` come with `blocked` (what it waits for, and the question) and `error`
//! (the message); `clear` says the agent has left the terminal. Everything is bounded: a field
//! over its limit, a state this parser does not know, or a message that is not base64 or not
//! UTF-8 is dropped with its field (never the whole report, unless it is the state). Unknown
//! keys are ignored, so a newer agent's extra fields do not break an older Hutan. The message
//! is **not** redacted here — that belongs to whoever shows it (the daemon does).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

/// Longest `app` kept, in bytes.
pub const MAX_APP_LEN: usize = 64;
/// Longest `kind` kept, in bytes.
pub const MAX_KIND_LEN: usize = 32;
/// Longest decoded `msg` kept, in characters (the sender's own limit).
pub const MAX_MESSAGE_CHARS: usize = 2048;
/// Longest base64 text of a message that is decoded: `MAX_MESSAGE_CHARS` of 4-byte characters.
const MAX_MESSAGE_B64: usize = MAX_MESSAGE_CHARS * 4 / 3 * 4 + 8;

/// What the agent says it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPhase {
    /// A turn is in progress.
    Working,
    /// It waits for the user (`kind` says for what, the message is the question).
    Blocked,
    /// The turn ended well.
    Done,
    /// The turn ended in an error (the message says which).
    Error,
    /// It waits for the next prompt.
    Idle,
}

/// One OSC 7501 report, `state=clear` excluded (that is [`crate::TerminalMeta::AgentCleared`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub phase: AgentPhase,
    /// The agent program as it names itself (`omp`).
    pub app: Option<String>,
    /// What a `blocked` agent waits for (`permission`, `question`, …), as the agent names it.
    pub kind: Option<String>,
    /// The question of a `blocked` agent or the message of an `error`.
    pub message: Option<String>,
}

/// What the payload after `7501;` says: a report, or the end of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    Status(AgentStatus),
    Cleared,
}

/// A word the agent uses for itself or for a kind: letters, digits and `. _ -`, within `max`.
fn word(value: &[u8], max: usize) -> Option<String> {
    (!value.is_empty()
        && value.len() <= max
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    .then(|| String::from_utf8_lossy(value).into_owned())
}

/// The message of a report: base64 (padding optional) of UTF-8 text, cut to the limit.
fn message(value: &[u8]) -> Option<String> {
    if value.is_empty() || value.len() > MAX_MESSAGE_B64 {
        return None;
    }
    let trimmed = value
        .strip_suffix(b"==")
        .or_else(|| value.strip_suffix(b"="));
    let decoded = STANDARD_NO_PAD.decode(trimmed.unwrap_or(value)).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(MAX_MESSAGE_CHARS)
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Parses what follows `7501;`.
pub(crate) fn parse(payload: &[u8]) -> Option<Parsed> {
    let mut phase = None;
    let mut cleared = false;
    let (mut app, mut kind, mut msg) = (None, None, None);
    for field in payload.split(|&b| b == b':') {
        let Some(at) = field.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = (&field[..at], &field[at + 1..]);
        match key {
            b"state" => match value {
                b"working" => phase = Some(AgentPhase::Working),
                b"blocked" => phase = Some(AgentPhase::Blocked),
                b"done" => phase = Some(AgentPhase::Done),
                b"error" => phase = Some(AgentPhase::Error),
                b"idle" => phase = Some(AgentPhase::Idle),
                b"clear" => cleared = true,
                _ => return None,
            },
            b"app" => app = word(value, MAX_APP_LEN),
            b"kind" => kind = word(value, MAX_KIND_LEN),
            b"msg" => msg = message(value),
            _ => {}
        }
    }
    if cleared {
        return Some(Parsed::Cleared);
    }
    Some(Parsed::Status(AgentStatus {
        phase: phase?,
        app,
        kind,
        message: msg,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(payload: &[u8]) -> AgentStatus {
        match parse(payload) {
            Some(Parsed::Status(status)) => status,
            other => panic!("not a status: {other:?}"),
        }
    }

    #[test]
    fn every_state_of_the_omp_format() {
        for (text, phase) in [
            ("working", AgentPhase::Working),
            ("blocked", AgentPhase::Blocked),
            ("done", AgentPhase::Done),
            ("error", AgentPhase::Error),
            ("idle", AgentPhase::Idle),
        ] {
            let got = status(format!("state={text}:app=omp").as_bytes());
            assert_eq!(got.phase, phase);
            assert_eq!(got.app.as_deref(), Some("omp"));
            assert_eq!((got.kind, got.message), (None, None));
        }
    }

    #[test]
    fn blocked_with_a_kind_and_a_question() {
        // "Run the tests?" as OMP writes it.
        let got = status(b"state=blocked:kind=permission:app=omp:msg=UnVuIHRoZSB0ZXN0cz8=");
        assert_eq!(got.phase, AgentPhase::Blocked);
        assert_eq!(got.kind.as_deref(), Some("permission"));
        assert_eq!(got.message.as_deref(), Some("Run the tests?"));
    }

    #[test]
    fn a_message_without_padding_and_a_multibyte_one() {
        // "ok" without its `=`; "привет ✓".
        assert_eq!(
            status(b"state=error:app=omp:msg=b2s").message.as_deref(),
            Some("ok")
        );
        assert_eq!(
            status(b"state=error:app=omp:msg=0L/RgNC40LLQtdGCIOKckw==")
                .message
                .as_deref(),
            Some("привет ✓")
        );
    }

    #[test]
    fn clear_is_the_end_of_the_agent() {
        assert_eq!(parse(b"state=clear"), Some(Parsed::Cleared));
        assert_eq!(parse(b"state=clear:app=omp"), Some(Parsed::Cleared));
    }

    #[test]
    fn field_order_unknown_keys_and_a_missing_app_are_tolerated() {
        let got = status(b"app=claude:future=1:state=working");
        assert_eq!(got.phase, AgentPhase::Working);
        assert_eq!(got.app.as_deref(), Some("claude"));
        assert_eq!(status(b"state=idle").app, None);
    }

    #[test]
    fn what_is_not_a_report_is_dropped() {
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"app=omp"), None);
        assert_eq!(parse(b"state=sleeping:app=omp"), None);
        assert_eq!(parse(b"nonsense"), None);
    }

    #[test]
    fn a_bad_field_loses_only_itself() {
        // Not base64, not UTF-8, an app that is too long or not a word.
        assert_eq!(status(b"state=error:app=omp:msg=!!!").message, None);
        assert_eq!(status(b"state=error:app=omp:msg=/w==").message, None);
        let long = format!("state=idle:app={}", "a".repeat(MAX_APP_LEN + 1));
        assert_eq!(status(long.as_bytes()).app, None);
        assert_eq!(status(b"state=idle:app=o m p").app, None);
        assert_eq!(status(b"state=blocked:kind=a/b:app=omp").kind, None);
    }

    #[test]
    fn a_message_is_bounded_and_stripped_of_controls() {
        let long = "x".repeat(MAX_MESSAGE_CHARS + 500);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&long);
        let got = status(format!("state=error:app=omp:msg={encoded}").as_bytes());
        assert_eq!(
            got.message.map(|m| m.chars().count()),
            Some(MAX_MESSAGE_CHARS)
        );
        // ESC and BEL cannot be smuggled through a message into a client's text.
        let encoded = base64::engine::general_purpose::STANDARD.encode("a\x1b[31mb\x07c");
        let got = status(format!("state=error:app=omp:msg={encoded}").as_bytes());
        assert_eq!(got.message.as_deref(), Some("a[31mbc"));
        // A message far beyond the limit is not even decoded.
        let huge = "A".repeat(MAX_MESSAGE_B64 + 4);
        assert_eq!(
            status(format!("state=error:app=omp:msg={huge}").as_bytes()).message,
            None
        );
    }
}
