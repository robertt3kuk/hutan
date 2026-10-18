//! `daemon.toml` over the protocol (protocol 0.28, capability `daemon.settings`): the daemon's
//! own options as a list a client can show and change (`docs/protocol.md` § Daemon settings).
//!
//! The daemon owns `daemon.toml`. `daemon.settings.get` answers every supported key with its
//! value, default and whether a change applies at once or after a daemon restart;
//! `daemon.settings.set` validates, writes the file (comments and order kept) and answers the
//! list again. Values are never secrets: credentials stay in the Keychain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `daemon.settings.get` (params `{}`) and the answer of `daemon.settings.set`.
///
/// Errors: `invalid_state` when `daemon.toml` cannot be parsed (the message names the file, line
/// and column; `set` refuses to overwrite such a file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// The daemon.toml path (shown to the user, "Reveal in Finder").
    pub file: String,
    pub entries: Vec<DaemonSetting>,
    /// True when some entry's file value is not what the running daemon uses.
    pub restart_pending: bool,
}

/// One option of `daemon.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonSetting {
    /// Dotted daemon.toml path: `notify.enabled`, `assistant.context.files`,
    /// `assistant.workspaces.<id>.files`, `harness.omp.bin`, `mcp.<name>` …
    pub key: String,
    pub kind: DaemonSettingKind,
    /// The value in daemon.toml, else the default (JSON of the TOML value).
    pub value: Value,
    pub default: Value,
    /// Present in daemon.toml (a reset removes it).
    pub set_in_file: bool,
    pub applies: Applies,
    /// The running daemon uses something else (an edit that needs a restart, or a file edited
    /// by hand since start).
    pub pending_restart: bool,
}

/// What kind of value a setting holds, and what the daemon accepts for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonSettingKind {
    Bool,
    Int {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    String,
    /// A file or executable path; the empty string removes the key (the default lookup).
    Path,
    Enum {
        choices: Vec<String>,
    },
    StringList {
        /// The allowed elements; absent: any string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<String>>,
    },
    /// A table of strings (environment variables).
    StringMap,
    /// Shown here, edited in the file (MCP servers).
    Table,
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// The running daemon uses the new value as soon as `daemon.settings.set` answers.
    Live,
    /// The daemon reads it at start: `pending_restart` is true until it restarts.
    Restart,
}

/// `daemon.settings.set`: `value` absent (or `null`) removes the key from `daemon.toml`, back to
/// the default; an empty string removes a `path` or `string` key too. Answered with
/// [`DaemonSettings`].
///
/// Errors: `bad_request` for a key that is not a setting, a value of the wrong type or outside
/// the range or choices, a path that does not exist or is not executable (`bin`, `cli`) or not a
/// directory (roots, `home_folder`), and for a `table` key; `invalid_state` when `daemon.toml`
/// cannot be parsed; `internal` when it cannot be written; `forbidden` from an
/// agent-originated connection, always (ADR 0016): an agent must not change its own limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonSettingSet {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}
