//! Loading durable state at start and the rules for what gets stored (ADR 0006, ADR 0009).

use std::collections::HashMap;
use std::path::PathBuf;

use hutan_core::{ObservationTree, SessionId, Task};
use hutan_protocol::messages::TerminalSpec;
use hutan_storage::{
    ClosedSession, ProviderRow, RecentProject, SessionRow, Storage, StorageError, WorkspaceRow,
};
use serde::{Deserialize, Serialize};

/// Everything the registry needs to rebuild its state.
#[derive(Debug, Default)]
pub struct Stored {
    pub workspaces: Vec<WorkspaceRow>,
    pub sessions: Vec<SessionRow>,
    /// The sessions `session.close` removed, most recently closed first.
    pub closed_sessions: Vec<ClosedSession>,
    /// Newest first.
    pub recent_projects: Vec<RecentProject>,
    /// What the daemon knew about the agents of each session when it stopped.
    pub agent_trees: HashMap<SessionId, ObservationTree>,
    /// Configured model providers, oldest first.
    pub providers: Vec<ProviderRow>,
    /// Tasks, oldest first, with their stored states (`todo`, `running`, `done`, `failed`,
    /// `cancelled`).
    pub tasks: Vec<Task>,
    /// What the database says about the run that ended (`session.recovery`).
    pub facts: crate::recovery::Facts,
}

/// Blocking: reads the whole durable state. Run it off the async threads.
pub fn load(storage: &Storage) -> Result<Stored, StorageError> {
    Ok(Stored {
        workspaces: storage.workspaces().list()?,
        sessions: storage.sessions().list()?,
        closed_sessions: storage.sessions().list_closed()?,
        recent_projects: storage.recent_projects().list()?,
        agent_trees: crate::tree::load(storage)?,
        providers: storage.providers().list()?,
        tasks: storage.tasks().list()?,
        facts: crate::recovery::Facts::default(),
    })
}

/// The folder a stored session started in: the spec's `cwd` (terminals), else the directory its
/// row remembers (what chat sessions have).
pub fn start_cwd(row: &SessionRow) -> Option<PathBuf> {
    serde_json::from_str::<serde_json::Value>(&row.spec_json)
        .ok()
        .and_then(|spec| {
            spec.get("cwd")
                .and_then(|cwd| cwd.as_str().map(PathBuf::from))
        })
        .or_else(|| row.cwd.clone())
}

/// Native resume reference of a session, as stored in `resume_ref_json`: written when a coding
/// agent reports a resume command (`agent.report`, see [`crate::agent`]), read back when the
/// session is respawned after a daemon restart.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResumeRef {
    /// Replaces the spec's command when the session is respawned after a daemon restart.
    pub argv: Vec<String>,
    /// The reporting source that owns this reference; only its `agent.release` clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
}

/// The reporting source recorded in a stored resume reference.
pub fn resume_source(resume_ref_json: &str) -> Option<String> {
    serde_json::from_str::<ResumeRef>(resume_ref_json)
        .ok()
        .and_then(|resume| resume.source)
}

/// The command a stored resume reference asks to run; `None` when absent, unreadable, or empty.
pub fn resume_argv(resume_ref_json: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<ResumeRef>(resume_ref_json) {
        Ok(resume) if !resume.argv.is_empty() => Some(resume.argv),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "ignoring an unreadable resume reference");
            None
        }
    }
}

/// Name fragments of environment variables that must never reach the database: secrets live in
/// the Keychain only (ADR 0008).
const SECRET_NAME_PARTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
    "ACCESS_KEY",
];

pub fn looks_secret(env_name: &str) -> bool {
    let upper = env_name.to_ascii_uppercase();
    SECRET_NAME_PARTS.iter().any(|part| upper.contains(part))
}

/// The spec as it goes to the database: environment variables with secret-looking names are
/// left out, so a restored session starts without them.
pub fn persistable_spec(spec: &TerminalSpec) -> TerminalSpec {
    let mut spec = spec.clone();
    spec.env.retain(|name, _| !looks_secret(name));
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_looking_names_are_recognised() {
        for name in [
            "GITHUB_TOKEN",
            "aws_secret_access_key",
            "DB_PASSWORD",
            "OPENAI_API_KEY",
        ] {
            assert!(looks_secret(name), "{name}");
        }
        for name in [
            "PATH",
            "PS1",
            "SSH_AUTH_SOCK",
            "KEYBOARD_LAYOUT",
            "HUTAN_SOCKET",
        ] {
            assert!(!looks_secret(name), "{name}");
        }
    }

    #[test]
    fn stored_specs_drop_secret_env() {
        let spec = TerminalSpec {
            env: [
                ("PS1".to_owned(), "$ ".to_owned()),
                ("NPM_TOKEN".to_owned(), "abc".to_owned()),
            ]
            .into(),
            ..TerminalSpec::default()
        };
        let stored = persistable_spec(&spec);
        assert_eq!(stored.env.len(), 1);
        assert!(stored.env.contains_key("PS1"));
    }

    #[test]
    fn resume_argv_reads_only_a_usable_command() {
        assert_eq!(
            resume_argv(r#"{"argv":["codex","resume","abc"]}"#),
            Some(vec!["codex".into(), "resume".into(), "abc".into()])
        );
        assert_eq!(resume_argv(r#"{"argv":[]}"#), None);
        assert_eq!(resume_argv("not json"), None);
        assert_eq!(resume_argv(r#"{"thread":"x"}"#), None);
    }
}
