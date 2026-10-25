//! Apple `container`: `container system status --format json` and `container system version
//! --format json`, parsed tolerantly.
//!
//! Shapes are the `Codable` payloads of the CLI source (`SystemStatus.swift`:
//! `StatusPayload{status, client, server, host, paths, resources}`, optional members are
//! omitted when absent) and the JSON example of `docs/command-reference.md` § `container
//! system version`. Unknown keys are ignored; counts of containers and images are never read.
//! `status` exits 1 unless the services run and still prints its JSON.

use std::path::Path;

use serde::Deserialize;

use crate::kind::RuntimeKind;
use crate::probe::HostProbe;
use crate::version::Version;

use super::{RuntimeReport, RuntimeState};

/// Apple `container` needs macOS 26 (technical overview; the formula says `macos >= 26`).
pub const MACOS_FLOOR: Version = Version::new(26, 0, 0);
/// Lowest version whose documentation and source this slice read (1.6.1). C-2 replaces it with
/// the range it actually tests; older releases are reported `Unsupported` until then.
pub const CONTAINER_FLOOR: Version = Version::new(1, 6, 1);

const START_HINT: &str = "the container services are not running; start them yourself with `container system start` (Hutan never does)";

/// `container system status` in one of its three states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppleStatus {
    /// "apiserver is not running and not registered with launchd".
    Unregistered,
    /// Registered but the health ping failed.
    NotRunning,
    Running {
        client_version: Option<String>,
        server_version: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct StatusPayload {
    status: String,
    client: Option<Component>,
    server: Option<Component>,
}

#[derive(Debug, Deserialize)]
struct Component {
    version: Option<String>,
}

/// Why a CLI output could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("not the expected JSON: {0}")]
    Json(String),
    #[error("unknown status {0:?}")]
    UnknownStatus(String),
}

pub fn parse_status(stdout: &str) -> Result<AppleStatus, ParseError> {
    let payload: StatusPayload =
        serde_json::from_str(stdout).map_err(|e| ParseError::Json(e.to_string()))?;
    match payload.status.as_str() {
        "unregistered" => Ok(AppleStatus::Unregistered),
        "not running" => Ok(AppleStatus::NotRunning),
        "running" => Ok(AppleStatus::Running {
            client_version: payload.client.and_then(|c| c.version),
            server_version: payload.server.and_then(|s| s.version),
        }),
        other => Err(ParseError::UnknownStatus(other.to_string())),
    }
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    #[serde(rename = "appName")]
    app_name: Option<String>,
    version: Option<String>,
}

/// Version of the CLI (`appName: "container"`) from `system version --format json`.
pub fn parse_cli_version(stdout: &str) -> Result<Option<Version>, ParseError> {
    let entries: Vec<VersionEntry> =
        serde_json::from_str(stdout).map_err(|e| ParseError::Json(e.to_string()))?;
    Ok(entries
        .iter()
        .find(|e| e.app_name.as_deref() == Some("container"))
        .and_then(|e| e.version.as_deref())
        .and_then(Version::find))
}

fn macos_version(probe: &dyn HostProbe) -> Option<Version> {
    let out = probe
        .run(Path::new("/usr/bin/sw_vers"), &["-productVersion"])
        .ok()?;
    Version::find(&out.stdout)
}

/// Is Apple `container` installed, supported and running? Reads only.
pub fn detect_apple(probe: &dyn HostProbe) -> RuntimeReport {
    let kind = RuntimeKind::AppleContainer;
    let mut report = RuntimeReport::new(kind, RuntimeState::Missing, probe.now_ms());
    let Some(cli) = probe.find_executable("container") else {
        return report;
    };
    report.cli = Some(cli.clone());

    match macos_version(probe) {
        Some(v) if v >= MACOS_FLOOR => {}
        Some(v) => {
            report.state = RuntimeState::Unsupported {
                reason: format!("needs macOS {MACOS_FLOOR} or later; this Mac runs {v}"),
            };
            return report;
        }
        None => {
            report.state = RuntimeState::Unsupported {
                reason: "cannot read the macOS version (`sw_vers -productVersion`)".to_string(),
            };
            return report;
        }
    }

    let status = match probe.run(&cli, &["system", "status", "--format", "json"]) {
        Ok(out) => parse_status(&out.stdout).map_err(|e| {
            format!(
                "unreadable `container system status` output (exit {:?}): {e}",
                out.status
            )
        }),
        Err(e) => Err(format!("`container system status` did not run: {e}")),
    };
    let status = match status {
        Ok(status) => status,
        Err(reason) => {
            report.state = RuntimeState::Unsupported { reason };
            return report;
        }
    };

    // The CLI answers `system version` without the services; the running server's own version
    // (a longer string) comes with the status.
    let (state, version) = match status {
        AppleStatus::Running {
            client_version,
            server_version,
        } => {
            let text = server_version.or(client_version);
            (
                RuntimeState::Running,
                text.as_deref().and_then(Version::find),
            )
        }
        AppleStatus::Unregistered | AppleStatus::NotRunning => {
            let cli_version = match probe.run(&cli, &["system", "version", "--format", "json"]) {
                Ok(out) => parse_cli_version(&out.stdout).ok().flatten(),
                Err(_) => None,
            };
            (
                RuntimeState::InstalledNotRunning {
                    hint: START_HINT.to_string(),
                },
                cli_version,
            )
        }
    };
    report.version = version.map(|v| v.to_string());
    report.state = match version {
        Some(v) if v < CONTAINER_FLOOR => RuntimeState::Unsupported {
            reason: format!("container {v} is older than the tested floor {CONTAINER_FLOOR}"),
        },
        _ => state,
    };
    report
}
