//! Which runtime, and which command-line dialect it speaks.

use std::fmt;

/// The Docker-compatible products Hutan recognises (spike `2026-10-26-containers` § 1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Product {
    OrbStack,
    DockerDesktop,
    Colima,
    RancherDesktop,
    Podman,
    /// An Engine API socket that answers but belongs to none of the above (a Lima docker
    /// template, a plain Linux-style `dockerd`, …).
    Other,
}

impl Product {
    /// The products detection reports on even when nothing is installed, in report order.
    pub const LISTED: [Product; 5] = [
        Product::OrbStack,
        Product::DockerDesktop,
        Product::Colima,
        Product::RancherDesktop,
        Product::Podman,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Product::OrbStack => "orbstack",
            Product::DockerDesktop => "docker-desktop",
            Product::Colima => "colima",
            Product::RancherDesktop => "rancher-desktop",
            Product::Podman => "podman",
            Product::Other => "docker",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Product::OrbStack => "OrbStack",
            Product::DockerDesktop => "Docker Desktop",
            Product::Colima => "Colima",
            Product::RancherDesktop => "Rancher Desktop",
            Product::Podman => "Podman",
            Product::Other => "Docker-compatible engine",
        }
    }
}

/// A runtime adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuntimeKind {
    /// Apple `container` (VM per container, macOS 26+).
    AppleContainer,
    /// Anything reached through the `docker` / `podman` CLI and the Engine API.
    DockerCompatible(Product),
}

impl RuntimeKind {
    /// Apple first, then the listed products (the order of a detection report).
    pub fn listed() -> impl Iterator<Item = RuntimeKind> {
        std::iter::once(RuntimeKind::AppleContainer).chain(
            Product::LISTED
                .into_iter()
                .map(RuntimeKind::DockerCompatible),
        )
    }

    pub fn id(self) -> &'static str {
        match self {
            RuntimeKind::AppleContainer => "apple-container",
            RuntimeKind::DockerCompatible(product) => product.id(),
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            RuntimeKind::AppleContainer => "Apple container",
            RuntimeKind::DockerCompatible(product) => product.display_name(),
        }
    }

    pub fn dialect(self) -> Dialect {
        match self {
            RuntimeKind::AppleContainer => Dialect::Apple,
            RuntimeKind::DockerCompatible(Product::Podman) => Dialect::Podman,
            RuntimeKind::DockerCompatible(_) => Dialect::Docker,
        }
    }
}

/// Stable identifier of one runtime (`apple-container`, `orbstack`, `colima`, `colima-dev`
/// for a second Colima profile, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeId(pub String);

impl RuntimeId {
    pub fn of(kind: RuntimeKind) -> Self {
        Self(kind.id().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The command-line dialect of a runtime's CLI. Docker and Podman share one grammar; the
/// differences that matter here are the user flags, `delete` vs `rm`, and how containers are
/// listed (see `argv`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// `container …`
    Apple,
    /// `docker …`
    Docker,
    /// `podman …`
    Podman,
}

impl Dialect {
    /// Name of the CLI binary.
    pub fn binary(self) -> &'static str {
        match self {
            Dialect::Apple => "container",
            Dialect::Docker => "docker",
            Dialect::Podman => "podman",
        }
    }

    pub fn is_docker_like(self) -> bool {
        !matches!(self, Dialect::Apple)
    }
}
