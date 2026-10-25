//! The [`ContainerRuntime`] trait: what the daemon asks of one runtime adapter.
//!
//! Most of it *builds launches* for Hutan's existing seams (a PTY command for the terminal
//! crate, a stdio command for `Transport::spawn_v2`); only a few calls talk to the runtime.
//! The lifecycle methods return `impl Future + Send` so no executor is chosen here and no
//! `dyn` async is needed: the daemon dispatches over an `enum` of adapters (Apple in C-2,
//! Docker-compatible in C-5). C-1 ships the trait and everything the adapters share; it has no
//! implementation yet.

use std::time::Duration;

use crate::caps::{CapsError, RuntimeCaps};
use crate::cli::{Cli, Launch};
use crate::detect::RuntimeReport;
use crate::kind::RuntimeId;
use crate::probe::HostProbe;
use crate::spec::{
    ContainerId, ContainerRef, ContainerSpec, ContainerState, ExecSpec, ImageRef, SpecError,
    VolumeName,
};

/// Failure of a lifecycle call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Caps(#[from] CapsError),
    /// Never pulled implicitly: the user must supply the image.
    #[error("image {0} is not present locally (Hutan never pulls or builds implicitly)")]
    ImageMissing(ImageRef),
    #[error("runtime {0} is not running")]
    NotRunning(RuntimeId),
    /// The CLI ran and failed.
    #[error("`{command}` failed (exit {status:?}): {stderr}")]
    Command {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    /// The CLI's output was not what its documentation says.
    #[error("unreadable output of `{command}`: {reason}")]
    Output { command: String, reason: String },
    #[error("{0}")]
    Io(String),
}

pub trait ContainerRuntime: Send + Sync {
    fn id(&self) -> &RuntimeId;

    /// The launcher for this runtime's CLI.
    fn cli(&self) -> &Cli;

    fn caps(&self) -> RuntimeCaps;

    /// Side-effect free (see [`crate::detect`]); `sandbox.runtimes` calls it. Blocking: run it on
    /// a blocking thread.
    fn detect(&self, probe: &dyn HostProbe) -> RuntimeReport;

    /// Idempotent by label `hutan.session=<id>`: create and start, or return the one that
    /// exists. Checks the image locally first ([`RuntimeError::ImageMissing`]) and refuses a
    /// spec the runtime's caps cannot honour.
    fn ensure(
        &self,
        spec: &ContainerSpec,
    ) -> impl Future<Output = Result<ContainerId, RuntimeError>> + Send;

    fn inspect(
        &self,
        id: &ContainerId,
    ) -> impl Future<Output = Result<ContainerState, RuntimeError>> + Send;

    /// Create the volume if it does not exist (idempotent).
    fn volume_ensure(
        &self,
        name: &VolumeName,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    fn stop(
        &self,
        id: &ContainerId,
        grace: Duration,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    fn remove(
        &self,
        id: &ContainerId,
        force: bool,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    /// Every container labelled `hutan.managed`, for the daemon-start reaper.
    fn list_hutan(&self) -> impl Future<Output = Result<Vec<ContainerRef>, RuntimeError>> + Send;

    /// `<cli> exec -it …`: the daemon owns the PTY (resize, OSC 133/7 injection and the resource
    /// manager stay as they are); the spawned process is the CLI.
    fn exec_pty(&self, id: &ContainerId, cmd: &ExecSpec) -> Launch {
        self.cli().exec_pty(id, cmd)
    }

    /// `<cli> exec -i …` for stdio peers (ACP, `codex app-server`, the relay).
    fn exec_stdio(&self, id: &ContainerId, cmd: &ExecSpec) -> Launch {
        self.cli().exec_stdio(id, cmd)
    }
}
