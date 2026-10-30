//! The log of agents' states (protocol 0.66, ADR 0025 decision 3, `docs/assistant.md` § Flow).
//!
//! The registry says here, whenever the state an agent reports about itself changes, what it
//! became and who said so. The log is the only way to tell afterwards how long a *terminal*
//! agent waited for the user: the daemon keeps nothing but the current state of such an agent.
//! It holds the state word, the source, what a blocked agent waits for and a short message —
//! redacted by the history index's redactor and cut to [`MESSAGE_CHARS`] — never output, and
//! rows go after 14 days. `[agents] state_log = false` stops the writing and deletes what the
//! log holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hutan_core::SessionId;
use hutan_storage::{AgentStateRow, Storage};
use tracing::warn;

/// The longest message a row keeps, in characters.
pub const MESSAGE_CHARS: usize = 120;

/// The writer of the log, shared by the registry and the settings.
#[derive(Debug)]
pub struct StateLog {
    storage: Storage,
    enabled: AtomicBool,
}

impl StateLog {
    pub fn new(storage: Storage, enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            storage,
            enabled: AtomicBool::new(enabled),
        })
    }

    /// Whether states are being written.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turns the writing on or off. Turning it off deletes what the log holds (applied before
    /// this returns).
    pub async fn set_enabled(&self, on: bool) {
        let was = self.enabled.swap(on, Ordering::Relaxed);
        if was && !on {
            let storage = self.storage.clone();
            let cleared =
                tokio::task::spawn_blocking(move || storage.agent_state_log().clear()).await;
            if !matches!(cleared, Ok(Ok(()))) {
                warn!("could not empty the state log: {cleared:?}");
            }
        }
    }

    /// Records that `session`'s agent is now in `state` as `source` says. Nothing happens
    /// while the log is off.
    pub fn record(
        &self,
        session: SessionId,
        at_ms: u64,
        source: &str,
        state: &str,
        kind: Option<&str>,
        message: Option<&str>,
    ) {
        if !self.enabled() {
            return;
        }
        let row = AgentStateRow {
            session_id: session,
            at_ms,
            source: source.chars().take(64).collect(),
            state: state.to_owned(),
            kind: kind.map(|k| k.chars().take(40).collect()),
            message: message.map(message_text).filter(|m| !m.is_empty()),
        };
        if let Err(error) = self.storage.agent_state_log().append(row) {
            warn!(%error, "could not write the state log");
        }
    }
}

/// One line of at most [`MESSAGE_CHARS`] characters, redacted before it is cut.
fn message_text(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let redacted = hutan_index::redact::redact(&line);
    if redacted.chars().count() <= MESSAGE_CHARS {
        redacted.into_owned()
    } else {
        let cut: String = redacted.chars().take(MESSAGE_CHARS - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_one_redacted_line_of_at_most_120_characters() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let text = message_text(&format!("deploy\n with   {secret}"));
        assert_eq!(text, "deploy with [REDACTED]");
        let long = message_text(&"x".repeat(500));
        assert_eq!(long.chars().count(), MESSAGE_CHARS);
        assert!(long.ends_with('…'));
        assert_eq!(message_text("   "), "");
    }
}
