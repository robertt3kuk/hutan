//! What a runtime can honour, and the fail-closed check of a spec against it.
//!
//! The table is static knowledge per [`RuntimeKind`], read from vendor documentation and source
//! (spike `2026-10-26-containers` §§ 1.2–1.4); nothing here was measured on a running runtime.
//! Where the sources do not settle a capability, the entry is the **closed** one (`false`,
//! `Unsupported`, `Flaky`, `Unknown`) and the comment says what C-2/C-5 must test before it may
//! be opened. A policy that needs a capability the runtime lacks is refused, never degraded.

use crate::kind::{Product, RuntimeId, RuntimeKind};
use crate::spec::{BindKind, ContainerSpec, Mount, NetworkPolicy};

/// How strong the isolation around a container is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Boundary {
    /// Not known: assume nothing.
    Unknown,
    /// Containers share one VM and kernel (OrbStack, Docker Desktop, Colima, Rancher, Podman).
    SharedVm,
    /// One lightweight VM per container (Apple `container`).
    VmPerContainer,
}

impl Boundary {
    /// The words the UI chip uses (ADR 0021 decision 6).
    pub fn label(self) -> &'static str {
        match self {
            Boundary::Unknown => "unknown isolation",
            Boundary::SharedVm => "shared VM",
            Boundary::VmPerContainer => "VM per container",
        }
    }
}

/// How far a feature can be relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reliability {
    Reliable,
    /// Works sometimes (Apple single-file mounts, apple/container #1251).
    Flaky,
    Unsupported,
}

/// The capabilities Hutan's policies depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCaps {
    pub boundary: Boundary,
    /// `--network none` gives a container without any network interface.
    pub network_none: bool,
    /// A host-only (`--internal`) network exists.
    pub network_internal: bool,
    /// Whether the host (and every host service listening on all addresses) stays reachable
    /// from an internal network. `true` is the pessimistic answer: Apple's gateway stays
    /// reachable (apple/container #1320); for the Docker family nothing says otherwise.
    pub host_reachable_from_internal: bool,
    /// A host unix socket can be mounted into the guest (Apple only, § 1.4).
    pub host_socket_mount: bool,
    pub single_file_mount: Reliability,
    /// A mount inside another mount's tree works: a read-only `.git/hooks` under a writable
    /// repo mount, a volume for `target/` under the worktree. (README name: `nested_ro_mounts`.)
    pub nested_mounts: bool,
    pub named_volumes: bool,
    pub rosetta: bool,
    /// `TIOCSWINSZ` on the exec's PTY reaches the process in the guest.
    pub exec_resize: bool,
}

impl RuntimeCaps {
    pub fn for_kind(kind: RuntimeKind) -> Self {
        match kind {
            // Sources: docs/technical-overview.md (VM per container), command-reference.md
            // (`--mount`, `--cap-drop`, `--read-only`, `--init`, `--rosetta`, `--publish-socket`,
            // `container volume`), apple/container #1037 (a user runs `--network none`),
            // #1320 (host gateway reachable from `--internal`), #1251 (single files).
            RuntimeKind::AppleContainer => RuntimeCaps {
                boundary: Boundary::VmPerContainer,
                network_none: true,
                network_internal: true,
                host_reachable_from_internal: true,
                host_socket_mount: true,
                single_file_mount: Reliability::Flaky,
                // CLOSED until C-2 runs a nested mount (read-only `hooks/`, a volume under a
                // bind mount) on a real 1.6.x: the sources neither promise nor exclude it.
                nested_mounts: false,
                named_volumes: true,
                rosetta: true,
                // CLOSED until C-2's resize test (README § 4.2: "works from 1.6.1 [INFERENCE]").
                exec_resize: false,
            },
            // Stock Docker Engine semantics (`--network none`, overlapping bind mounts, named
            // volumes, `exec -t` forwarding SIGWINCH) as documented by Docker; per-product
            // facts from the vendors' pages in the spike § 1.
            RuntimeKind::DockerCompatible(product) => RuntimeCaps {
                boundary: match product {
                    Product::Other => Boundary::Unknown,
                    _ => Boundary::SharedVm,
                },
                network_none: true,
                network_internal: true,
                host_reachable_from_internal: true,
                // OrbStack discussion #584, colima #997: not generally supported.
                host_socket_mount: false,
                single_file_mount: Reliability::Reliable,
                nested_mounts: true,
                named_volumes: true,
                // OrbStack and Docker Desktop ship Rosetta; Colima needs `--vz-rosetta`,
                // Rancher and Podman are opt-in or unreported.
                rosetta: matches!(product, Product::OrbStack | Product::DockerDesktop),
                exec_resize: true,
            },
        }
    }

    /// What `spec` (plus an optional boundary the policy insists on) needs that `self` lacks.
    pub fn missing_for(
        &self,
        spec: &ContainerSpec,
        required_boundary: Option<Boundary>,
    ) -> Vec<MissingCap> {
        let mut missing = Vec::new();
        if spec.network == NetworkPolicy::None && !self.network_none {
            missing.push(MissingCap::NetworkNone);
        }
        if spec
            .mounts
            .iter()
            .any(|m| matches!(m, Mount::Volume { .. }))
            && !self.named_volumes
        {
            missing.push(MissingCap::NamedVolumes);
        }
        let nested = spec.mounts.iter().any(|inner| {
            spec.mounts
                .iter()
                .any(|outer| inner.guest().is_inside(outer.guest()))
        });
        if nested && !self.nested_mounts {
            missing.push(MissingCap::NestedMounts);
        }
        let single_file = spec.mounts.iter().any(|m| {
            matches!(
                m,
                Mount::Bind {
                    kind: BindKind::File,
                    ..
                }
            )
        });
        if single_file && self.single_file_mount != Reliability::Reliable {
            missing.push(MissingCap::SingleFileMount);
        }
        if let Some(required) = required_boundary
            && self.boundary < required
        {
            missing.push(MissingCap::Boundary {
                required,
                actual: self.boundary,
            });
        }
        missing
    }
}

/// One capability a spec needs and the runtime does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingCap {
    NetworkNone,
    NamedVolumes,
    NestedMounts,
    SingleFileMount,
    Boundary {
        required: Boundary,
        actual: Boundary,
    },
}

impl std::fmt::Display for MissingCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MissingCap::NetworkNone => f.write_str("`--network none` (network_none)"),
            MissingCap::NamedVolumes => f.write_str("named volumes (named_volumes)"),
            MissingCap::NestedMounts => {
                f.write_str("mounts nested inside another mount (nested_mounts)")
            }
            MissingCap::SingleFileMount => {
                f.write_str("reliable single-file bind mounts (single_file_mount)")
            }
            MissingCap::Boundary { required, actual } => {
                write!(
                    f,
                    "a {} boundary (the runtime offers: {})",
                    required.label(),
                    actual.label()
                )
            }
        }
    }
}

/// The runtime cannot honour the policy: the session must not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("runtime {runtime} cannot honour this policy; missing: {}", list(.missing))]
pub struct CapsError {
    pub runtime: RuntimeId,
    pub missing: Vec<MissingCap>,
}

fn list(missing: &[MissingCap]) -> String {
    missing
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
