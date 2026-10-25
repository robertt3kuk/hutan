//! Hutan's container runtime layer (slice C-1; spike `docs/spikes/2026-10-26-containers/`).
//!
//! A container is a *placement* for terminals and agents (ADR 0021 decision 7). This crate is
//! the part that needs no runtime installed and is not yet wired into the daemon (wiring is C-2):
//!
//! - [`spec`]: validated value types and [`ContainerSpec`], [`ExecSpec`];
//! - [`argv`] and [`Cli`]: pure argv builders for Apple `container` and Docker/Podman CLIs;
//! - [`detect`]: read-only detection (Apple `container system status`, Docker Engine API
//!   `/_ping`, `/version`, `/info`, `podman machine inspect`, app bundles) through an injected
//!   [`HostProbe`], with [`SystemProbe`] as the real one;
//! - [`caps`] and [`policy`]: what each runtime can honour and the fail-closed policy → spec step;
//! - [`ContainerRuntime`]: the trait the adapters implement.
//!
//! No GPUI, no daemon types, no `unsafe`, no async runtime. Nothing here ever starts, pulls or
//! runs anything on the machine; a runtime is used only through [`ContainerRuntime`] by an
//! adapter, and only after [`detect`] found it running.

pub mod argv;
pub mod caps;
mod cli;
pub mod detect;
mod kind;
pub mod policy;
pub mod probe;
mod runtime;
pub mod spec;
mod system;
mod version;

pub use caps::{Boundary, CapsError, MissingCap, Reliability, RuntimeCaps};
pub use cli::{Cli, Launch};
pub use detect::{RuntimeReport, RuntimeState, detect_all};
pub use kind::{Dialect, Product, RuntimeId, RuntimeKind};
pub use policy::{ContainerPolicy, PolicyError, VolumeMount};
pub use probe::{CommandOutput, HostProbe, HttpResponse, PathKind, ProbeError};
pub use runtime::{ContainerRuntime, RuntimeError};
pub use spec::{
    AbsPath, BindKind, ContainerId, ContainerRef, ContainerSpec, ContainerState, EnvVar, ExecIo,
    ExecSpec, ImageRef, Labels, Limits, Mount, NetworkPolicy, SpecError, User, VolumeName,
};
pub use system::SystemProbe;
pub use version::Version;
