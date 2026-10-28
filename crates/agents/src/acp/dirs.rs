//! The session's extra folders in an ACP session (protocol 0.57, ADR 0023 decision 4).
//!
//! Agents that advertise `sessionCapabilities.additionalDirectories` (Claude Code's adapter)
//! take the list in `session/new`, `session/load` and `session/resume`, and only there: ACP
//! defines no mid-session change, and an omitted list on load means none, so the list is sent
//! at every start. OMP accepts and ignores that field; it has the local commands `/add-dir`,
//! `/remove-dir` and `/dirs` instead — sent as prompts, answered by the agent without a model
//! call, kept in OMP's session header and inherited by a fork. Hutan sends them right after
//! the session opens (only what OMP's `/dirs` does not already list, on a resumed session) and
//! between turns, and keeps their output out of the conversation.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{Shared, lock, open_call_collecting, wire};
use crate::adapter::{AgentDirs, DirsChange};
use crate::error::AgentError;
use crate::jsonrpc::{Incoming, REQUEST_TIMEOUT, Transport};

/// The tag of a `session/prompt` that is a local command, not a turn
/// ([`Transport::request_ordered`]).
pub(super) const LOCAL_TAG: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// OMP: `/add-dir` and `/remove-dir` prompts.
    Commands,
    /// The session calls carry the list (Claude Code).
    AtOpen,
    /// The agent never advertised it.
    Never,
}

/// What an ACP session knows of its folders.
#[derive(Debug)]
pub(super) struct SessionDirs {
    how: How,
    wanted: Vec<PathBuf>,
    given: Vec<PathBuf>,
}

fn advertised(capability: &Option<Value>) -> bool {
    matches!(capability, Some(value) if !value.is_null() && *value != Value::Bool(false))
}

impl SessionDirs {
    pub(super) fn open(
        agent: &str,
        capabilities: &wire::AgentCapabilities,
        wanted: &[PathBuf],
    ) -> Self {
        let how = if agent == "omp" {
            How::Commands
        } else if advertised(&capabilities.session_capabilities.additional_directories) {
            How::AtOpen
        } else {
            How::Never
        };
        Self {
            how,
            wanted: wanted.to_vec(),
            given: if how == How::AtOpen {
                wanted.to_vec()
            } else {
                Vec::new()
            },
        }
    }

    /// What the session calls carry.
    pub(super) fn given(&self) -> &[PathBuf] {
        &self.given
    }

    pub(super) fn uses_commands(&self) -> bool {
        self.how == How::Commands
    }

    pub(super) fn state(&self) -> AgentDirs {
        AgentDirs {
            given: self.given.clone(),
            change: match self.how {
                How::Commands => DirsChange::NextTurn,
                How::AtOpen => DirsChange::Restart,
                How::Never => DirsChange::Unsupported,
            },
        }
    }

    fn to_add(&self) -> Vec<PathBuf> {
        self.wanted
            .iter()
            .filter(|dir| !self.given.contains(dir))
            .cloned()
            .collect()
    }

    fn to_remove(&self) -> Vec<PathBuf> {
        self.given
            .iter()
            .filter(|dir| !self.wanted.contains(dir))
            .cloned()
            .collect()
    }
}

/// A local command in flight: its output so far, and who waits for it.
#[derive(Debug)]
pub(super) struct LocalCommand {
    said: String,
    done: Option<oneshot::Sender<Result<String, AgentError>>>,
}

/// The text of an `agent_message_chunk` update.
pub(super) fn message_chunk(params: &Value) -> Option<&str> {
    let update = params.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    update.get("content")?.get("text")?.as_str()
}

/// A message chunk that belongs to the local command in flight: kept for it, not for the
/// conversation. `false` for everything else.
pub(super) fn keep_local_output(shared: &Shared, params: &Value) -> bool {
    let mut local = lock(&shared.local);
    let (Some(command), Some(text)) = (local.as_mut(), message_chunk(params)) else {
        return false;
    };
    command.said.push_str(text);
    true
}

/// The local command's prompt ended.
pub(super) fn finish_local(shared: &Shared, result: Result<Value, AgentError>) {
    let Some(mut command) = lock(&shared.local).take() else {
        debug!("a local command's answer arrived with nobody waiting");
        return;
    };
    if let Some(done) = command.done.take() {
        let _ = done.send(result.map(|_| command.said));
    }
}

fn prompt_params(session: &str, text: &str) -> Value {
    json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]})
}

/// Runs one local command while the pump runs. The caller holds `turn_active`.
async fn local_command(shared: &Shared, text: &str) -> Result<String, AgentError> {
    let (done, answer) = oneshot::channel();
    *lock(&shared.local) = Some(LocalCommand {
        said: String::new(),
        done: Some(done),
    });
    if let Err(error) = shared
        .transport
        .request_ordered(
            "session/prompt",
            prompt_params(&shared.session_id, text),
            LOCAL_TAG,
        )
        .await
    {
        *lock(&shared.local) = None;
        return Err(error);
    }
    match timeout(REQUEST_TIMEOUT, answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(AgentError::Exited {
            detail: shared.transport.stderr_tail(),
        }),
        Err(_) => {
            *lock(&shared.local) = None;
            Err(AgentError::Timeout {
                method: "session/prompt".to_owned(),
            })
        }
    }
}

fn add_command(path: &Path) -> String {
    format!("/add-dir {}", path.display())
}

fn remove_command(path: &Path) -> String {
    format!("/remove-dir {}", path.display())
}

/// Whether OMP's answer to `/add-dir` says the folder is in the session now: "Added /path." or
/// "Already in the workspace: /path" (OMP 18.12.3; anything else is a refusal such as
/// "Directory does not exist").
fn confirmed(said: &str) -> bool {
    let said = said.trim_start();
    said.starts_with("Added") || said.starts_with("Already in the workspace")
}

/// Whether OMP's answer to `/remove-dir` says the folder is out of the session now: "Removed
/// /path." or "Not a workspace directory: /path".
fn removed(said: &str) -> bool {
    let said = said.trim_start();
    said.starts_with("Removed") || said.starts_with("Not a workspace directory")
}

/// The extra folders of OMP's `/dirs` answer (`Workspace directories:` and one indented path
/// per line; the working directory is marked and left out).
pub(super) fn parse_dirs(said: &str) -> Vec<PathBuf> {
    said.lines()
        .skip_while(|line| !line.trim_start().starts_with("Workspace directories"))
        .skip(1)
        .filter(|line| line.starts_with(' '))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with("(working directory)"))
        .map(PathBuf::from)
        .collect()
}

/// The same, before the pump runs: what the session has after `session/new|load`.
pub(super) async fn open_with_commands(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    dirs: &mut SessionDirs,
    session: &str,
    resumed: bool,
    managed: bool,
    wanted: &[PathBuf],
) {
    async fn run(
        transport: &Transport,
        incoming: &mut mpsc::Receiver<Incoming>,
        session: &str,
        text: &str,
    ) -> Result<String, AgentError> {
        let mut said = String::new();
        open_call_collecting(
            transport,
            incoming,
            "session/prompt",
            prompt_params(session, text),
            Some(&mut said),
        )
        .await?;
        Ok(said)
    }
    let mut listed = Vec::new();
    // A conversation that never had a folder has nothing to reconcile: no command is spent on
    // it. One that had asks OMP, whose session header keeps what was added.
    if resumed && managed {
        match run(transport, incoming, session, "/dirs").await {
            Ok(said) => listed = parse_dirs(&said),
            Err(error) => warn!(%error, "could not ask the agent which folders it has"),
        }
    }
    for path in listed.iter().filter(|dir| !wanted.contains(dir)) {
        match run(transport, incoming, session, &remove_command(path)).await {
            Ok(said) if removed(&said) => {}
            Ok(said) => warn!(
                said = said.trim(),
                "the agent did not confirm `/remove-dir`"
            ),
            Err(error) => warn!(%error, "could not take a folder out of the agent's session"),
        }
    }
    for path in wanted {
        if listed.contains(path) {
            dirs.given.push(path.clone());
            continue;
        }
        match run(transport, incoming, session, &add_command(path)).await {
            Ok(said) if confirmed(&said) => dirs.given.push(path.clone()),
            Ok(said) => {
                warn!(said = said.trim(), "the agent did not confirm `/add-dir`");
            }
            Err(error) => warn!(%error, "could not add a folder to the agent's session"),
        }
    }
}

/// Gives OMP what is wanted and not given yet, and takes away what is no longer wanted. The
/// caller holds `turn_active`. Stops at the first failure; what was done stays done.
pub(super) async fn sync_held(shared: &Shared) -> Result<(), AgentError> {
    let (add, remove) = {
        let dirs = lock(&shared.dirs);
        if !dirs.uses_commands() {
            return Ok(());
        }
        (dirs.to_add(), dirs.to_remove())
    };
    for path in remove {
        let said = local_command(shared, &remove_command(&path)).await?;
        if !removed(&said) {
            return Err(AgentError::Protocol(format!(
                "the agent did not confirm `/remove-dir`: {}",
                said.trim()
            )));
        }
        lock(&shared.dirs).given.retain(|dir| *dir != path);
    }
    for path in add {
        let said = local_command(shared, &add_command(&path)).await?;
        if !confirmed(&said) {
            return Err(AgentError::Protocol(format!(
                "the agent did not confirm `/add-dir`: {}",
                said.trim()
            )));
        }
        lock(&shared.dirs).given.push(path);
    }
    Ok(())
}

/// `set_dirs`: records the wanted folders, and gives them to OMP at once when no turn runs.
pub(super) async fn set(shared: &Shared, dirs: Vec<PathBuf>) -> Result<(), AgentError> {
    lock(&shared.dirs).wanted = dirs;
    if !lock(&shared.dirs).uses_commands() {
        return Ok(());
    }
    // A running turn (or another local command) holds the flag: the folders reach the agent
    // before the next message.
    if shared
        .turn_active
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return Ok(());
    }
    let outcome = sync_held(shared).await;
    shared
        .turn_active
        .store(false, std::sync::atomic::Ordering::Release);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_lists_the_extra_folders_and_leaves_the_working_directory_out() {
        let said = "Workspace directories:\n  /tmp/a (working directory)\n  /tmp/b\n  /tmp/c d\n";
        assert_eq!(
            parse_dirs(said),
            [PathBuf::from("/tmp/b"), PathBuf::from("/tmp/c d")]
        );
        assert!(parse_dirs("Workspace directories:\n  /tmp/a (working directory)").is_empty());
        assert!(parse_dirs("nothing to see").is_empty());
    }

    #[test]
    fn the_answers_of_add_dir_and_remove_dir_are_told_apart_by_their_first_words() {
        assert!(confirmed("Added /tmp/b.\nWorkspace directories:\n  /tmp/a"));
        assert!(confirmed("Already in the workspace: /tmp/b"));
        assert!(!confirmed("Directory does not exist: /tmp/b"));
        assert!(!confirmed("Not a directory: /tmp/b"));
        assert!(!confirmed(""));
        assert!(removed("Removed /tmp/b.\nWorkspace directories:\n  /tmp/a"));
        assert!(removed("Not a workspace directory: /tmp/b"));
        assert!(!removed("Added /tmp/b."));
    }

    #[test]
    fn the_message_chunk_of_an_update_is_its_text() {
        let params = json!({"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}});
        assert_eq!(message_chunk(&params), Some("hi"));
        let other = json!({"update": {"sessionUpdate": "usage_update"}});
        assert_eq!(message_chunk(&other), None);
    }
}
