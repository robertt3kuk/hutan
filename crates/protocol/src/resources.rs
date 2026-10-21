//! The resource manager (protocol 0.18, capability `resources`): DTOs of `resources.*`,
//! `process.signal`, `agent.interrupt`, `session.kill` and the `resources.updated` event
//! (`docs/protocol.md` § Resources, `docs/resources.md`).
//!
//! Errors, in short: `not_found` (an unknown session, or a node the session's agent tree does
//! not have), `invalid_state` (the session is not running; no turn to interrupt; this harness
//! cannot stop a single subagent), `forbidden` (a pid that is not the session's process or one
//! of its descendants, `hutand` itself, pid 1), `bad_request` (an interval below the minimum).

use hutan_core::{NodeKind, NodeState, SessionId};
use serde::{Deserialize, Serialize};

/// Shortest sampling interval a subscriber may ask for.
pub const MIN_INTERVAL_MS: u32 = 1_000;
/// Interval used when `resources.subscribe` names none.
pub const DEFAULT_INTERVAL_MS: u32 = 2_000;
/// Most processes listed for one session; beyond it the session is `truncated`.
pub const MAX_PROCESSES_PER_SESSION: usize = 512;
/// Most processes listed over all sessions; beyond it the sample is `truncated`.
pub const MAX_PROCESSES_TOTAL: usize = 4_096;
/// Longest [`ProcessInfo::command`], in characters.
pub const MAX_COMMAND_CHARS: usize = 512;

/// What the kernel says a process is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Sleeping,
    Stopped,
    Zombie,
    /// Being created.
    Idle,
    Unknown,
}

/// One process of a session's tree, or `hutand` itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    /// The executable's name.
    pub name: String,
    /// The command line, at most [`MAX_COMMAND_CHARS`] characters, with secret-looking
    /// arguments (`--token=…`, `API_KEY=…`, bearer credentials) replaced by `[REDACTED]`.
    pub command: String,
    pub state: ProcessState,
    /// Share of one CPU core since the previous sample (`100.0` = one full core, more with
    /// several busy threads). Absent in the first sample of a process: there is nothing to
    /// compare with yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// Resident memory, in bytes.
    pub rss_bytes: u64,
    pub threads: u32,
    /// Start time, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// The node of the session's agent tree that reported this pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_node: Option<String>,
}

/// What one session's processes add up to.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ResourceTotals {
    pub processes: u32,
    /// Sum of the processes' `cpu_percent`; absent processes count as zero.
    pub cpu_percent: f64,
    pub rss_bytes: u64,
}

/// An agent of a session's tree that has not finished, with its pid when it reported one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub kind: NodeKind,
    /// The node's name, else its type, else its id.
    pub label: String,
    pub state: NodeState,
    /// Absent when the agent reported none, or the pid is not (any more) in the session's
    /// process tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// The processes of one session: the terminal's child and its descendants, or a chat
/// session's harness process and its descendants. A direct chat has none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionResources {
    pub session_id: SessionId,
    /// Parents before their children, siblings by pid.
    pub processes: Vec<ProcessInfo>,
    pub totals: ResourceTotals,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentProcess>,
    /// Processes beyond [`MAX_PROCESSES_PER_SESSION`] (or the sample's total limit) are left
    /// out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// What a service the daemon runs for itself (not for a session) adds up to: a language server
/// and the processes below it (protocol 0.39).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceResources {
    /// `lsp:` and the server's id.
    pub service_id: String,
    /// What runs: `lsp`.
    pub kind: String,
    /// The service's name: `rust-analyzer` and its folder.
    pub label: String,
    /// Parents before their children.
    pub processes: Vec<ProcessInfo>,
    pub totals: ResourceTotals,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// One look at everything Hutan runs: the live sessions, the daemon's services and `hutand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceSample {
    pub at_ms: u64,
    /// Live sessions that have a process, in the order the sessions were created.
    pub sessions: Vec<SessionResources>,
    /// The language servers that run (protocol 0.39): `service` nodes next to the sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceResources>,
    /// `hutand` itself.
    pub daemon: ProcessInfo,
    /// Some process was left out because of [`MAX_PROCESSES_TOTAL`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `resources.list`: one fresh sample, of one session or of all. Answered with
/// [`ResourceSample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourcesListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// `resources.subscribe`: samples the processes every `interval_ms` (at least
/// [`MIN_INTERVAL_MS`], default [`DEFAULT_INTERVAL_MS`]) and sends this connection a
/// `resources.updated` event each time, until `resources.unsubscribe` or the connection ends.
/// Nothing is sampled while no connection is subscribed. Answered with [`ResourcesSubscribed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourcesSubscribe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u32>,
}

/// Result of `resources.subscribe`: the interval in force for this connection and the first
/// sample (its CPU figures are absent when nobody had sampled before).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourcesSubscribed {
    pub interval_ms: u32,
    pub sample: ResourceSample,
}

/// Signals `process.signal` sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalName {
    Term,
    Kill,
    Int,
    Hup,
    Stop,
    Cont,
}

impl SignalName {
    pub const ALL: [SignalName; 6] = [
        SignalName::Term,
        SignalName::Kill,
        SignalName::Int,
        SignalName::Hup,
        SignalName::Stop,
        SignalName::Cont,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SignalName::Term => "term",
            SignalName::Kill => "kill",
            SignalName::Int => "int",
            SignalName::Hup => "hup",
            SignalName::Stop => "stop",
            SignalName::Cont => "cont",
        }
    }
}

/// `process.signal`: signals one process of a session. `pid` must be the session's own process
/// or one of its descendants at the time of the call, checked against a fresh look at the
/// process table (`forbidden` otherwise; never `hutand`, never pid 1). Answered with
/// [`ProcessSignaled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSignal {
    pub session_id: SessionId,
    pub pid: u32,
    pub signal: SignalName,
}

/// Result of `process.signal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSignaled {
    pub session_id: SessionId,
    pub pid: u32,
    pub signal: SignalName,
}

/// `agent.interrupt`: stops what an agent of a session is doing, without ending the session.
/// Answered with [`AgentInterrupted`].
///
/// - Without `node_id` (or with the main agent's), the session's main agent: a chat session's
///   running turn (Codex `turn/interrupt`, direct chat stop), or, in a terminal session, the
///   ^C a user would type (a terminal agent cancels its turn on it).
/// - With `node_id`, a subagent: a Codex subagent thread is interrupted with `turn/interrupt`
///   on that thread; a node whose pid is known (and still in the session's process tree)
///   gets SIGINT, and with `force` SIGTERM two seconds later if it is still running. Any other
///   node is `invalid_state`, naming that this harness cannot stop a single subagent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterrupt {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// How an interrupt was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptVia {
    /// The harness was asked to stop the turn of the main agent.
    Turn,
    /// The harness was asked to stop the turn of a subagent's thread.
    Thread,
    /// ^C was typed into the session's terminal.
    Keystroke,
    /// SIGINT was sent to the agent's process.
    Sigint,
    /// SIGINT was sent and, the process still running after the grace, SIGTERM.
    SigintTerm,
}

/// Result of `agent.interrupt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterrupted {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub via: InterruptVia,
    /// The process that was signalled, for the process ways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}
