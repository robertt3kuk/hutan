//! What a daemon start did to each session it found (protocol 0.26, capability
//! `session.recovery`): the facts the start has, and the rules that turn them into
//! [`Recovery`] values. Nothing here guesses: a program that did not run through the restart is
//! never reported as running (ADR 0009).

use std::collections::{BTreeMap, HashMap, HashSet};

use hutan_core::{SessionId, SessionKind};
use hutan_protocol::recovery::{DaemonStop, LastTurn, Lost, Recovery, RecoveryKind};
use hutan_storage::{
    ApprovalRow, ChatTurnStatus, EpochEnd, EpochRecord, QuestionRow, SessionRow, Storage,
};
use tracing::warn;

use crate::chat::{DAEMON_STOP_DECIDER, DAEMON_STOP_TURN_ERROR};

/// `app_meta` key of the commands that run in terminals (session → command line, JSON).
const RUNNING_COMMANDS_KEY: &str = "recovery.running_commands";
/// Longest command line kept, in characters.
const COMMAND_MAX_CHARS: usize = 200;

/// What the database says about the run that ended and the sessions it left.
#[derive(Debug, Default)]
pub struct Facts {
    /// The latest run recorded before this one.
    pub previous: Option<EpochRecord>,
    /// Chat sessions whose last turn the previous run cut off.
    pub interrupted_turns: HashSet<SessionId>,
    /// Permission requests of each session that expired with the previous run.
    pub lost_approvals: HashMap<SessionId, u32>,
    /// Questions of each session that expired with the previous run.
    pub lost_questions: HashMap<SessionId, u32>,
    /// The command each terminal was running in the foreground, as last recorded.
    pub running_commands: HashMap<SessionId, String>,
    /// The state each session's agent was last logged in (`(source, state)`): what this run
    /// must not log again when the same agent reports the same state after the restart.
    pub last_states: HashMap<SessionId, (String, String)>,
}

/// Blocking: reads the facts. `expired` are the approvals a crashed run left open, as
/// [`crate::chat::expire_dangling_approvals`] found them. Run before this daemon records its
/// own epoch.
pub fn load(
    storage: &Storage,
    sessions: &[SessionRow],
    expired: &[ApprovalRow],
    expired_questions: &[QuestionRow],
) -> Result<Facts, hutan_storage::StorageError> {
    let previous = storage.app_meta().epoch_history()?.last().copied();
    let mut lost_approvals: HashMap<SessionId, u32> = HashMap::new();
    for row in expired {
        *lost_approvals.entry(row.session_id).or_default() += 1;
    }
    let mut lost_questions: HashMap<SessionId, u32> = HashMap::new();
    for row in expired_questions {
        *lost_questions.entry(row.session_id).or_default() += 1;
    }
    if let Some(previous) = &previous {
        // Questions a clean stop expired on its way out.
        for row in storage.questions().resolved_since(previous.started_at_ms)? {
            if row.decided_by.as_deref() == Some(DAEMON_STOP_DECIDER) {
                *lost_questions.entry(row.session_id).or_default() += 1;
            }
        }
        // Approvals a clean stop expired on its way out.
        for row in storage.approvals().resolved_since(previous.started_at_ms)? {
            if row.decided_by.as_deref() == Some(DAEMON_STOP_DECIDER) {
                *lost_approvals.entry(row.session_id).or_default() += 1;
            }
        }
    }
    let mut interrupted_turns = HashSet::new();
    for row in sessions
        .iter()
        .filter(|row| row.kind == SessionKind::Harness && row.state.is_live())
    {
        if last_turn_was_cut_off(storage, row.id, previous.as_ref())? {
            interrupted_turns.insert(row.id);
        }
    }
    Ok(Facts {
        previous,
        interrupted_turns,
        lost_approvals,
        lost_questions,
        running_commands: load_running_commands(storage),
        last_states: storage
            .agent_state_log()
            .latest_before(u64::MAX)?
            .into_iter()
            .map(|row| (row.session_id, (row.source, row.state)))
            .collect(),
    })
}

/// Whether the newest turn of the session was cut off by the previous run: it never ended
/// (the daemon died), or a shutdown cancelled it. A turn from an earlier run that never
/// ended was already reported by the restart after it.
fn last_turn_was_cut_off(
    storage: &Storage,
    session: SessionId,
    previous: Option<&EpochRecord>,
) -> Result<bool, hutan_storage::StorageError> {
    let Some(last) = storage.chat().turns(session)?.pop() else {
        return Ok(false);
    };
    let cut_off = match last.status {
        ChatTurnStatus::Running => {
            previous.is_none_or(|run| last.started_at_ms >= run.started_at_ms)
        }
        ChatTurnStatus::Cancelled => {
            last.error.as_deref() == Some(DAEMON_STOP_TURN_ERROR)
                && previous.is_none_or(|run| last.ended_at_ms.unwrap_or(0) >= run.started_at_ms)
        }
        ChatTurnStatus::Completed | ChatTurnStatus::Failed => false,
    };
    Ok(cut_off)
}

/// How the run before this one ended.
pub fn daemon_stop(previous: Option<&EpochRecord>) -> DaemonStop {
    match previous.map(|run| run.end) {
        Some(EpochEnd::Clean { at_ms }) => DaemonStop::Clean { at_ms },
        Some(EpochEnd::Open) => DaemonStop::Crashed,
        Some(EpochEnd::Untracked) | None => DaemonStop::Unknown,
    }
}

/// The start the recovery values of one daemon start have in common.
#[derive(Debug, Clone)]
pub struct Context {
    pub previous_epoch: Option<hutan_core::Epoch>,
    pub daemon_stop: DaemonStop,
    pub restarted_at_ms: u64,
    /// When the previous run began: a session that ended after it ended "before the restart".
    pub previous_started_at_ms: Option<u64>,
}

impl Context {
    pub fn new(previous: Option<&EpochRecord>, restarted_at_ms: u64) -> Self {
        Self {
            previous_epoch: previous.map(|run| run.epoch),
            daemon_stop: daemon_stop(previous),
            restarted_at_ms,
            previous_started_at_ms: previous.map(|run| run.started_at_ms),
        }
    }

    pub fn recovery(&self, kind: RecoveryKind, lost: Vec<Lost>) -> Recovery {
        Recovery {
            kind,
            previous_epoch: self.previous_epoch,
            daemon_stop: self.daemon_stop,
            restarted_at_ms: self.restarted_at_ms,
            lost,
        }
    }

    /// The recovery of a session that was not live when the previous run ended: only one that
    /// ended during that run is listed as having ended before the restart.
    pub fn ended(&self, ended_at_ms: Option<u64>) -> Option<Recovery> {
        let ended = ended_at_ms?;
        let began = self.previous_started_at_ms?;
        (ended >= began).then(|| self.recovery(RecoveryKind::EndedBefore, Vec::new()))
    }
}

/// The `Lost` entries of a session, from the facts.
pub fn lost(facts: &Facts, session: SessionId, turn_cut_off: bool) -> Vec<Lost> {
    let mut lost = Vec::new();
    if let Some(&count) = facts.lost_approvals.get(&session) {
        lost.push(Lost::Approvals { count });
    }
    if let Some(&count) = facts.lost_questions.get(&session) {
        lost.push(Lost::Questions { count });
    }
    if turn_cut_off {
        lost.push(Lost::Turn);
    }
    lost
}

pub fn last_turn(interrupted: bool) -> LastTurn {
    if interrupted {
        LastTurn::Interrupted
    } else {
        LastTurn::Completed
    }
}

/// A command line as it may be remembered: secrets removed, cut to [`COMMAND_MAX_CHARS`].
/// `None` for a blank one.
pub fn bounded_command(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let redacted = hutan_index::redact::redact(command);
    let mut chars = redacted.chars();
    let mut bounded: String = chars.by_ref().take(COMMAND_MAX_CHARS).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    Some(bounded)
}

fn load_running_commands(storage: &Storage) -> HashMap<SessionId, String> {
    let json = match storage.app_meta().get(RUNNING_COMMANDS_KEY) {
        Ok(Some(json)) => json,
        Ok(None) => return HashMap::new(),
        Err(error) => {
            warn!(%error, "could not read the recorded commands");
            return HashMap::new();
        }
    };
    match serde_json::from_str::<BTreeMap<String, String>>(&json) {
        Ok(map) => map
            .into_iter()
            .filter_map(|(id, command)| Some((id.parse().ok()?, command)))
            .collect(),
        Err(error) => {
            warn!(%error, "the recorded commands are unreadable");
            HashMap::new()
        }
    }
}

/// Stores the commands that run in terminals now (session → command line), replacing what was
/// stored.
pub fn store_running_commands(
    storage: &Storage,
    commands: impl Iterator<Item = (SessionId, String)>,
) {
    let map: BTreeMap<String, String> = commands
        .map(|(id, command)| (id.to_string(), command))
        .collect();
    match serde_json::to_string(&map) {
        Ok(json) => crate::registry::log_write(
            storage.app_meta().set(RUNNING_COMMANDS_KEY, &json),
            "the recorded commands",
        ),
        Err(error) => warn!(%error, "could not encode the recorded commands"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::Epoch;

    fn run(end: EpochEnd) -> EpochRecord {
        EpochRecord {
            epoch: Epoch::new(),
            started_at_ms: 1_000,
            end,
        }
    }

    #[test]
    fn the_end_of_the_previous_run_is_only_claimed_when_it_is_known() {
        assert_eq!(
            daemon_stop(Some(&run(EpochEnd::Clean { at_ms: 5 }))),
            DaemonStop::Clean { at_ms: 5 }
        );
        assert_eq!(daemon_stop(Some(&run(EpochEnd::Open))), DaemonStop::Crashed);
        assert_eq!(
            daemon_stop(Some(&run(EpochEnd::Untracked))),
            DaemonStop::Unknown
        );
        assert_eq!(daemon_stop(None), DaemonStop::Unknown);
    }

    #[test]
    fn only_sessions_that_ended_during_the_previous_run_are_listed_as_ended_before() {
        let context = Context::new(Some(&run(EpochEnd::Open)), 9_000);
        assert!(context.ended(Some(1_000)).is_some());
        assert!(context.ended(Some(999)).is_none(), "older than the run");
        assert!(context.ended(None).is_none());
        assert!(Context::new(None, 9_000).ended(Some(5_000)).is_none());
    }

    #[test]
    fn remembered_commands_lose_secrets_and_length() {
        assert_eq!(bounded_command("  \n"), None);
        assert_eq!(
            bounded_command(" npm run dev ").as_deref(),
            Some("npm run dev")
        );
        let secret =
            bounded_command("curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123' x")
                .unwrap();
        assert!(!secret.contains("abcdefghijklmnop"), "{secret}");
        let long = bounded_command(&"x".repeat(500)).unwrap();
        assert_eq!(long.chars().count(), COMMAND_MAX_CHARS + 1);
        assert!(long.ends_with('…'));
    }
}
