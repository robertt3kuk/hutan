//! `hutan telemetry status|enable <agent>|disable <agent>`: the loopback OTLP receiver that turns
//! an agent's own OpenTelemetry into insight (protocol 0.74, capability `agent.telemetry`,
//! ADR 0026; `docs/integrations.md` § Report your agent's stats to Hutan).
//!
//! `status` contacts nothing and shows no token. `enable` and `disable` set
//! `telemetry.agents.<agent>` of `daemon.toml` through `daemon.settings.set`, which is the
//! user's alone: refused for a command that runs inside an agent's session.

use hutan_protocol::telemetry::{ListenerState, TelemetryStatus};
use serde_json::json;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{TelemetryAgentArgs, TelemetryCommand};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn telemetry(&self, command: TelemetryCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_telemetry() {
            return Err(CliError::Failed(
                "this hutand does not know the OTLP receiver (it is older than protocol 0.74); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            TelemetryCommand::Status => {}
            TelemetryCommand::Enable(TelemetryAgentArgs { agent }) => {
                client
                    .daemon_setting_set(&format!("telemetry.agents.{agent}"), Some(json!(true)))
                    .await
                    .map_err(|e| self.fail(e))?;
            }
            TelemetryCommand::Disable(TelemetryAgentArgs { agent }) => {
                client
                    .daemon_setting_set(&format!("telemetry.agents.{agent}"), Some(json!(false)))
                    .await
                    .map_err(|e| self.fail(e))?;
            }
        }
        let status = client.telemetry_status().await.map_err(|e| self.fail(e))?;
        self.print(&status, || status_text(&status, now_ms()))
    }
}

fn status_text(status: &TelemetryStatus, now_ms: u64) -> String {
    let mut lines = Vec::new();
    lines.push(match (status.listener, status.port) {
        (ListenerState::Listening, Some(port)) => {
            format!("OpenTelemetry receiver: listening on 127.0.0.1:{port} (this computer only)")
        }
        (ListenerState::Listening, None) => "OpenTelemetry receiver: listening".to_owned(),
        (ListenerState::Off, _) => {
            "OpenTelemetry receiver: off (no socket; no agent is enabled)".to_owned()
        }
        (ListenerState::Down, _) => format!(
            "OpenTelemetry receiver: DOWN — {}",
            status
                .down_reason
                .as_deref()
                .unwrap_or("the port could not be bound")
        ),
    });
    let on: Vec<&str> = status
        .agents
        .iter()
        .filter(|agent| agent.enabled)
        .map(|agent| agent.id.as_str())
        .collect();
    let off: Vec<&str> = status
        .agents
        .iter()
        .filter(|agent| !agent.enabled)
        .map(|agent| agent.id.as_str())
        .collect();
    lines.push(format!(
        "agents on: {}",
        if on.is_empty() {
            "none".to_owned()
        } else {
            on.join(", ")
        }
    ));
    if !off.is_empty() {
        lines.push(format!(
            "agents off: {} (`hutan telemetry enable <agent>`)",
            off.join(", ")
        ));
    }
    if status.listener == ListenerState::Listening {
        lines.push(
            "terminals started from now on get HUTAN_OTLP_ENDPOINT and HUTAN_OTLP_TOKEN; \
             terminals already open keep what they had"
                .to_owned(),
        );
    }
    if status.sessions.is_empty() {
        lines.push("no session has reported yet".to_owned());
    } else {
        lines.push("sessions that reported:".to_owned());
        for session in &status.sessions {
            let ago = age_text(now_ms.saturating_sub(session.last_report_ms) / 1000);
            let mut what = session.agent.clone();
            if let Some(version) = &session.service_version {
                what.push(' ');
                what.push_str(version);
            }
            if let Some(model) = &session.model {
                what.push_str(&format!(" · {model}"));
            }
            lines.push(format!(
                "  {}  {what}  {} report{}, last {ago} ago",
                session.session_id,
                session.reports,
                if session.reports == 1 { "" } else { "s" }
            ));
        }
    }
    let c = status.counters;
    lines.push(format!(
        "requests: {} accepted, {} refused, {} dropped, {} ignored",
        c.accepted, c.rejected, c.dropped, c.ignored
    ));
    lines.join("\n")
}
