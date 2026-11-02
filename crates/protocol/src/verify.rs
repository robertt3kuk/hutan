//! Verification (protocol 0.71, capability `agents.verify`, ADR 0025, design
//! `docs/spikes/2026-11-02-checks.md` §4): Hutan runs a session's tests itself and keeps the exit
//! code, so "the tests pass" is a fact the daemon recorded and not the agent's word.
//!
//! - `agents.verify.plan` (Read) says what would run: the command, the folder, why that
//!   command, and whether it would be refused now.
//! - `agents.verify` (Mutate; the user's click runs it, an agent goes through the control gate,
//!   Mawas through her own, bound to the session like a message) runs the plan and answers when
//!   the run is over. A run belongs to the daemon, not to the connection that asked: it ends by
//!   itself at the timeout, and it is killed when the caller that is waiting for it goes away.
//! - `agents.verify.cancel` (Mutate) stops the run of a session.
//! - Events `agents.verify.started` and `agents.verify.ended` carry the run.
//!
//! The output is the **last** [`VERIFY_TAIL_BYTES`] of what the command wrote (stdout and stderr
//! together), redacted. A run is stored (`verify_runs`) and survives a restart.

use std::path::PathBuf;

use hutan_core::SessionId;
use serde::{Deserialize, Serialize};

/// The most bytes of output a run keeps (its last ones).
pub const VERIFY_TAIL_BYTES: usize = 8 * 1024;
/// The longest command, in characters: a longer one is `bad_request`.
pub const VERIFY_COMMAND_CHARS: usize = 2_000;

/// `agents.verify.plan`: what `agents.verify` would run for a session. Answered with
/// [`VerifyPlan`]. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsVerifyPlanRequest {
    pub session_id: SessionId,
    /// The command to run instead of the project's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// `agents.verify`: run the tests of a session in its folder (its worktree when it has one).
/// Answered with [`VerifyRun`] once the run is over.
///
/// Errors: `not_found` (an unknown session), `invalid_state` (the session has no folder or is
/// closed; no test command found, the message says what was looked for; a run of this session is
/// in progress; the session's own turn is running — `data.verify.refused` is `turn_running` — and
/// `force` is not set), `bad_request` (an empty or overlong command), `forbidden` (`force` from
/// an agent: only the user forces).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsVerifyRequest {
    pub session_id: SessionId,
    /// The command to run instead of the project's own. It is run by the user's login shell,
    /// as typed. A caller that is an agent always asks for it, whatever was allowed before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Run although the session's own turn is running in the same checkout (the run races the
    /// agent). The user's alone: an agent asking for it is refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// `agents.verify.cancel`: kills the run of a session (its whole process tree). The waiting
/// `agents.verify` answers with status `cancelled`. Answered with `Empty`; `not_found` when no
/// run of that session is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsVerifyCancel {
    pub session_id: SessionId,
}

/// Where the command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifySource {
    /// The caller named it (`--command`).
    Caller,
    /// `[checks.projects.<name or id>] test_command` of `daemon.toml`.
    Project,
    /// `[checks] test_command` of `daemon.toml`.
    Setting,
    /// Found by the files of the folder ([`VerifyPlan::why`] names the file).
    Detected,
}

impl VerifySource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Caller => "caller",
            Self::Project => "project",
            Self::Setting => "setting",
            Self::Detected => "detected",
        }
    }
}

/// Why a plan would be refused now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyRefusal {
    /// A run of this session is in progress.
    Running,
    /// The session's own turn is running in this checkout; `force` (the user's) overrides.
    TurnRunning,
}

/// What would run, before it runs. The card of Mawas's request and the CLI show these words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyPlan {
    pub session_id: SessionId,
    /// The command, as the login shell is given it.
    pub command: String,
    /// The folder it runs in.
    pub cwd: PathBuf,
    pub source: VerifySource,
    /// In words: which setting, which file, or that the caller gave it.
    pub why: String,
    /// Set when the run would be refused now; absent when it would start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<VerifyRefusal>,
    /// How long the run may take before it is killed.
    pub timeout_secs: u64,
}

/// How a run ended (or that it has not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    Running,
    /// Exit code 0.
    Passed,
    /// A non-zero exit code, or a signal that was not ours.
    Failed,
    /// Killed at the timeout.
    TimedOut,
    /// Killed by `agents.verify.cancel` or because the caller went away.
    Cancelled,
    /// The command could not be started (`VerifyRun::error` says why).
    Error,
}

impl VerifyStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "running" => Self::Running,
            "passed" => Self::Passed,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// Who asked for the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyRequester {
    /// A person (the desktop's click, the CLI in a shell).
    User,
    /// Mawas, after the user answered her card.
    Mawas,
    /// Another agent, through the control gate.
    Agent,
}

impl VerifyRequester {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Mawas => "mawas",
            Self::Agent => "agent",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "user" => Self::User,
            "mawas" => Self::Mawas,
            "agent" => Self::Agent,
            _ => return None,
        })
    }
}

/// One run: in progress (`agents.verify.started`) or over (the answer of `agents.verify`, the
/// event `agents.verify.ended`, the row in `verify_runs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRun {
    /// The run's number in `verify_runs`; stable, increasing.
    pub id: u64,
    pub session_id: SessionId,
    pub command: String,
    pub cwd: PathBuf,
    pub source: VerifySource,
    pub why: String,
    pub status: VerifyStatus,
    /// The exit code; absent while running, when a signal ended the command, or when it never
    /// started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal that ended the command, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The last [`VERIFY_TAIL_BYTES`] of the output, redacted. Empty in `started`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_tail: String,
    /// The command wrote more than the tail holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_truncated: bool,
    pub requested_by: VerifyRequester,
    /// The user ran it although the session's own turn was running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forced: bool,
    /// Why a run with status `error` could not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VerifyRun {
    /// The run as the CLI and the MCP tool print it: the command, then the result and the tail.
    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = format!(
            "$ {}\n(in {}, {})\n",
            self.command,
            self.cwd.display(),
            self.why
        );
        match self.status {
            VerifyStatus::Running => text.push_str("running\n"),
            VerifyStatus::Passed => {
                let _ = writeln!(text, "passed: exit code 0{}", self.took());
            }
            VerifyStatus::Failed => {
                let how = match (self.exit_code, self.signal) {
                    (Some(code), _) => format!("exit code {code}"),
                    (None, Some(signal)) => format!("signal {signal}"),
                    (None, None) => "no exit code".to_owned(),
                };
                let _ = writeln!(text, "failed: {how}{}", self.took());
            }
            VerifyStatus::TimedOut => {
                let _ = writeln!(text, "timed out: killed{}", self.took());
            }
            VerifyStatus::Cancelled => {
                let _ = writeln!(text, "cancelled: killed{}", self.took());
            }
            VerifyStatus::Error => {
                let _ = writeln!(
                    text,
                    "could not run: {}",
                    self.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
        if !self.output_tail.is_empty() {
            let _ = writeln!(
                text,
                "--- {}output ---\n{}",
                if self.output_truncated {
                    "last of the "
                } else {
                    ""
                },
                self.output_tail.trim_end()
            );
        }
        text
    }

    fn took(&self) -> String {
        match self.duration_ms {
            Some(ms) => format!(" after {}.{} s", ms / 1000, (ms % 1000) / 100),
            None => String::new(),
        }
    }
}
