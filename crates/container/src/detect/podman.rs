//! `podman machine inspect` (JSON array of machines).
//!
//! Shape: the example of `podman-machine-inspect(1)` — `Name`, `State` (`running`, `stopped`, …)
//! and `ConnectionInfo.PodmanSocket.Path`, the host path of the machine's API socket.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodmanMachine {
    pub name: String,
    pub running: bool,
    /// The machine's Docker/Libpod API socket on the host.
    pub socket: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "ConnectionInfo")]
    connection: Option<Connection>,
}

#[derive(Debug, Deserialize)]
struct Connection {
    #[serde(rename = "PodmanSocket")]
    socket: Option<Socket>,
}

#[derive(Debug, Deserialize)]
struct Socket {
    #[serde(rename = "Path")]
    path: Option<String>,
}

/// Machines in `stdout`; empty for anything that is not the array (no machine exists: the
/// command fails with an error on stderr and prints nothing or `[]`).
pub fn parse_machines(stdout: &str) -> Vec<PodmanMachine> {
    let raw: Vec<Raw> = serde_json::from_str(stdout).unwrap_or_default();
    raw.into_iter()
        .map(|m| PodmanMachine {
            name: m.name,
            running: m.state.eq_ignore_ascii_case("running"),
            socket: m
                .connection
                .and_then(|c| c.socket)
                .and_then(|s| s.path)
                .filter(|p| p.starts_with('/'))
                .map(PathBuf::from),
        })
        .collect()
}
