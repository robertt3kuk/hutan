//! Read-only runtime detection (spike `2026-10-26-containers` § 3).
//!
//! Rules: on demand (the Start sheet's *Place in* row, Settings → Sandbox, `sandbox.runtimes`),
//! never at daemon start; each probe bounded by the [`HostProbe`] (2 s); callers cache a
//! report for at most 30 s. Detection never launches an app, starts a service, selects a
//! Docker context, pulls, builds or runs — it `stat`s paths, runs *query* subcommands
//! (`sw_vers`, `container system status|version`, `podman machine inspect`) and `GET`s
//! `/_ping`, `/version`, `/info`. A runtime is offered only when [`RuntimeState::Running`].

pub mod apple;
pub mod docker;
pub mod podman;

use std::path::PathBuf;

use crate::caps::RuntimeCaps;
use crate::cli::Cli;
use crate::kind::{RuntimeId, RuntimeKind};
use crate::probe::HostProbe;

pub use apple::detect_apple;
pub use docker::detect_docker_family;

/// Where a runtime stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    /// Installed (or its socket file is left over) but not answering; `hint` is for the user to
    /// act on — Hutan never starts a runtime.
    InstalledNotRunning {
        hint: String,
    },
    Missing,
    /// Present but unusable: too old, wrong OS, no CLI, unreadable output.
    Unsupported {
        reason: String,
    },
}

/// What detection found about one runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReport {
    pub id: RuntimeId,
    pub kind: RuntimeKind,
    pub state: RuntimeState,
    /// Engine / apiserver version, when it could be read.
    pub version: Option<String>,
    /// The CLI Hutan would drive.
    pub cli: Option<PathBuf>,
    /// The Engine API socket (Docker family only).
    pub endpoint: Option<PathBuf>,
    pub caps: RuntimeCaps,
    pub checked_at_ms: u64,
}

impl RuntimeReport {
    pub fn new(kind: RuntimeKind, state: RuntimeState, checked_at_ms: u64) -> Self {
        Self {
            id: RuntimeId::of(kind),
            kind,
            state,
            version: None,
            cli: None,
            endpoint: None,
            caps: RuntimeCaps::for_kind(kind),
            checked_at_ms,
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == RuntimeState::Running
    }

    /// The launcher for a running runtime; `None` otherwise.
    pub fn cli(&self) -> Option<Cli> {
        if !self.is_running() {
            return None;
        }
        let mut cli = Cli::new(self.kind.dialect(), self.cli.clone()?);
        cli.endpoint = self.endpoint.clone();
        Some(cli)
    }
}

/// Every listed runtime (Apple first), plus any unrecognised engine that answered.
pub fn detect_all(probe: &dyn HostProbe) -> Vec<RuntimeReport> {
    let mut reports = vec![detect_apple(probe)];
    reports.extend(detect_docker_family(probe));
    reports
}
