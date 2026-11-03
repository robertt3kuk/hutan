//! Agent telemetry (protocol 0.74, capability `agent.telemetry`, ADR 0026): the DTO of
//! `telemetry.status` (`docs/protocol.md` § Agent telemetry, `docs/integrations.md` § Report
//! your agent's stats to Hutan).
//!
//! The daemon can host a loopback OTLP receiver that turns an agent's own OpenTelemetry into
//! insight (`InsightSourceKind::Otlp`). It is off until the user enables an agent
//! (`[telemetry.agents] <agent> = true` of `daemon.toml`, set through `daemon.settings.set` —
//! user-only — or `hutan telemetry enable <agent>`). The status says whether the listener is
//! there, on which port, which agents are on and what each session last reported. It never
//! carries a session's token, an attribute of a payload or a path.

use hutan_core::SessionId;
use serde::{Deserialize, Serialize};

/// Where the receiver stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerState {
    /// No agent is enabled: there is no socket.
    Off,
    /// Bound to `127.0.0.1:<port>`.
    Listening,
    /// An agent is enabled but the port could not be bound (`down_reason` says why). The daemon
    /// never binds another port in its place: the port is in the agents' own configuration.
    Down,
}

/// One agent Hutan knows a mapper for, and whether the user turned it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryAgent {
    /// The name in `[telemetry.agents]`: `claude`, `codex`, `omp`, `gemini`, `qwen`, `copilot`,
    /// `goose`, `opencode`, `other` (any program that follows the OpenTelemetry GenAI
    /// conventions).
    pub id: String,
    /// What a person calls it.
    pub label: String,
    pub enabled: bool,
}

/// What one session last reported through the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetrySession {
    pub session_id: SessionId,
    /// The agent the records named (`service.name`), as an agent id of [`TelemetryAgent`].
    pub agent: String,
    /// When the last request that mapped to insight arrived.
    pub last_report_ms: u64,
    /// Requests of this session that mapped to insight since it started.
    pub reports: u64,
    /// The agent's own version (`service.version`), when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    /// The model the records named last (not a secret: a model id such as `claude-opus-5-5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// What the receiver has done since the daemon started: every request is counted once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TelemetryCounters {
    /// Answered `200` (a request is answered before it is mapped).
    pub accepted: u64,
    /// Refused before the body was read or decoded: not `POST`, a wrong `Host`, a content type
    /// that is not OTLP, no or a wrong token, too large, over a rate or connection limit.
    pub rejected: u64,
    /// Accepted and then not mapped: the mapper's queue was full (answered `429`), the body did
    /// not decode, or records past the cap.
    pub dropped: u64,
    /// Accepted and decoded, but nothing in it was Hutan's to map: metrics, an agent the user did
    /// not enable, records of no known shape.
    pub ignored: u64,
}

/// `telemetry.status`: the receiver's state. Read-only; contacts nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryStatus {
    pub listener: ListenerState,
    /// The port of `[telemetry] port` once the daemon has picked or been given one; absent
    /// before (`port = 0` and the receiver never started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Why the listener is [`ListenerState::Down`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_reason: Option<String>,
    /// Every agent with a mapper, enabled or not.
    pub agents: Vec<TelemetryAgent>,
    /// The sessions that reported, newest first; at most 64.
    pub sessions: Vec<TelemetrySession>,
    pub counters: TelemetryCounters,
}
