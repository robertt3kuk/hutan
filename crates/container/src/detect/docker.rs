//! Docker Engine-compatible runtimes, found by their API sockets.
//!
//! Probe order (spike § 3): `stat` the known socket paths, then `GET /_ping`, `/version` and
//! `/info` on each socket that exists — read-only Engine API endpoints that need no auth and
//! spawn no helper (unlike the `docker` CLI, which may run credential helpers). Podman's own
//! socket comes from `podman machine inspect`. Apps that are installed but not running are
//! recognised by their bundle (`/Applications/*.app`) or binary and are **never** launched.
//!
//! Who answers is decided from what the engine says about itself, and only then from the
//! socket path:
//! - `Server: Libpod/…` on any response, or a `Podman Engine` component in `/version`
//!   (podman `pkg/api/server/handler_api.go`, `handlers/compat/version.go`) → Podman;
//! - `/info` `OperatingSystem: Docker Desktop` (a real `docker info` of Docker Desktop 4.67)
//!   → Docker Desktop;
//! - a kernel whose version names `orbstack` (OrbStack's own boot log and a user's
//!   `docker info`: `6.1.23-orbstack-…`) → OrbStack;
//! - `/info` `Name` of `colima…` (a real `docker info` under Colima) or `lima-rancher-desktop`
//!   (the hostname set by Rancher Desktop's `lima-config.yaml`) → Colima / Rancher Desktop.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::kind::{Product, RuntimeId, RuntimeKind};
use crate::probe::{HostProbe, PathKind, ProbeError};
use crate::version::Version;

use super::podman::{PodmanMachine, parse_machines};
use super::{RuntimeReport, RuntimeState};

#[derive(Debug, Default, Deserialize)]
pub struct EngineVersion {
    #[serde(rename = "Version")]
    pub version: Option<String>,
    #[serde(rename = "ApiVersion")]
    pub api_version: Option<String>,
    #[serde(rename = "MinAPIVersion")]
    pub min_api_version: Option<String>,
    #[serde(rename = "Platform")]
    pub platform: Option<Platform>,
    #[serde(rename = "Components", default)]
    pub components: Vec<Component>,
    #[serde(rename = "Os")]
    pub os: Option<String>,
    #[serde(rename = "Arch")]
    pub arch: Option<String>,
    #[serde(rename = "KernelVersion")]
    pub kernel_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Platform {
    #[serde(rename = "Name")]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Component {
    #[serde(rename = "Name")]
    pub name: Option<String>,
}

/// The few `/info` (`SystemInfo`) fields that say who is behind the socket. Counts, image
/// lists and the engine ID are not read.
#[derive(Debug, Default, Deserialize)]
pub struct EngineInfo {
    #[serde(rename = "Name")]
    pub name: Option<String>,
    #[serde(rename = "ServerVersion")]
    pub server_version: Option<String>,
    #[serde(rename = "OperatingSystem")]
    pub operating_system: Option<String>,
    #[serde(rename = "KernelVersion")]
    pub kernel_version: Option<String>,
}

/// What one engine socket told us.
#[derive(Debug, Default)]
pub struct EngineFacts {
    /// `Server` header of the ping (`Docker/29.3.1 (linux)`, `Libpod/5.x (linux)`).
    pub server_header: Option<String>,
    pub version: Option<EngineVersion>,
    pub info: Option<EngineInfo>,
}

/// Why a socket is not a usable engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    /// The socket file exists and nothing accepts connections.
    Stale,
    /// Something answered that is not an Engine API (or timed out).
    NotAnEngine(String),
}

/// Ping, then best-effort `/version` and `/info`.
pub fn probe_endpoint(probe: &dyn HostProbe, socket: &Path) -> Result<EngineFacts, EndpointError> {
    let ping = match probe.http_get(socket, "/_ping") {
        Ok(r) => r,
        Err(ProbeError::Refused | ProbeError::NotFound) => return Err(EndpointError::Stale),
        Err(e) => return Err(EndpointError::NotAnEngine(e.to_string())),
    };
    if ping.status != 200 || ping.body.trim() != "OK" {
        return Err(EndpointError::NotAnEngine(format!(
            "`/_ping` answered {} {:?}",
            ping.status,
            ping.body.trim()
        )));
    }
    let json = |path: &str| {
        probe
            .http_get(socket, path)
            .ok()
            .filter(|r| r.status == 200)
            .map(|r| r.body)
    };
    Ok(EngineFacts {
        server_header: ping.header("Server").map(str::to_string),
        version: json("/version").and_then(|b| serde_json::from_str(&b).ok()),
        info: json("/info").and_then(|b| serde_json::from_str(&b).ok()),
    })
}

/// The product a socket path alone suggests (vendor-documented locations, spike § 3 probe 3).
pub fn product_from_path(socket: &Path) -> Option<Product> {
    let path = socket.to_string_lossy();
    if path.contains("/.orbstack/") {
        Some(Product::OrbStack)
    } else if path.contains("/.colima/") || path.contains("/colima/") {
        Some(Product::Colima)
    } else if path.contains("/.rd/") {
        Some(Product::RancherDesktop)
    } else if path.contains("/.docker/run/") || path.contains("/com.docker.docker/") {
        Some(Product::DockerDesktop)
    } else if path.to_ascii_lowercase().contains("podman") {
        Some(Product::Podman)
    } else {
        None
    }
}

/// Who answers on `socket`; `socket` should be the canonical path.
pub fn identify(socket: &Path, facts: &EngineFacts) -> Product {
    let kernel = facts
        .info
        .as_ref()
        .and_then(|i| i.kernel_version.as_deref())
        .or_else(|| {
            facts
                .version
                .as_ref()
                .and_then(|v| v.kernel_version.as_deref())
        })
        .unwrap_or("");
    let name = facts
        .info
        .as_ref()
        .and_then(|i| i.name.as_deref())
        .unwrap_or("");
    let os = facts
        .info
        .as_ref()
        .and_then(|i| i.operating_system.as_deref())
        .unwrap_or("");
    let platform = facts
        .version
        .as_ref()
        .and_then(|v| v.platform.as_ref())
        .and_then(|p| p.name.as_deref())
        .unwrap_or("");
    let podman_component = facts.version.as_ref().is_some_and(|v| {
        v.components
            .iter()
            .any(|c| c.name.as_deref() == Some("Podman Engine"))
    });

    if facts
        .server_header
        .as_deref()
        .is_some_and(|s| s.starts_with("Libpod/"))
        || podman_component
    {
        Product::Podman
    } else if os == "Docker Desktop" || platform.starts_with("Docker Desktop") {
        Product::DockerDesktop
    } else if kernel.to_ascii_lowercase().contains("orbstack") {
        Product::OrbStack
    } else if name.starts_with("colima") {
        Product::Colima
    } else if name.contains("rancher-desktop") {
        Product::RancherDesktop
    } else {
        product_from_path(socket).unwrap_or(Product::Other)
    }
}

fn engine_version(facts: &EngineFacts) -> Option<String> {
    facts
        .version
        .as_ref()
        .and_then(|v| v.version.clone())
        .or_else(|| facts.info.as_ref().and_then(|i| i.server_version.clone()))
        .or_else(|| {
            facts
                .server_header
                .as_deref()
                .and_then(Version::find)
                .map(|v| v.to_string())
        })
}

/// Sockets worth probing, in priority order, only those that exist as sockets.
fn socket_candidates(probe: &dyn HostProbe, podman: &[PodmanMachine]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(host) = probe.env_var("DOCKER_HOST")
        && let Some(path) = host.strip_prefix("unix://")
        && path.starts_with('/')
    {
        paths.push(PathBuf::from(path));
    }
    if let Some(home) = probe.home_dir() {
        paths.push(home.join(".docker/run/docker.sock")); // Docker Desktop
        paths.push(home.join(".orbstack/run/docker.sock")); // OrbStack
        for profile in probe.list_dir(&home.join(".colima")) {
            paths.push(profile.join("docker.sock")); // Colima, one directory per profile
        }
        // Newer Colima keeps profiles under XDG config (observed by a user in
        // testcontainers-go#2952).
        for profile in probe.list_dir(&home.join(".config/colima")) {
            paths.push(profile.join("docker.sock"));
        }
        paths.push(home.join(".rd/docker.sock")); // Rancher Desktop
        for instance in probe.list_dir(&home.join(".lima")) {
            paths.push(instance.join("sock/docker.sock")); // Lima's docker template
        }
    }
    paths.extend(
        podman
            .iter()
            .filter(|m| m.running)
            .filter_map(|m| m.socket.clone()),
    );
    paths.push(PathBuf::from("/var/run/docker.sock")); // a symlink whose target says who owns it
    paths.retain(|p| probe.path_kind(p) == PathKind::Socket);
    paths
}

/// Colima profile of a socket path (`…/colima/<profile>/docker.sock`), if any.
fn colima_profile(socket: &Path) -> Option<String> {
    let dir = socket.parent()?;
    let profile = dir.file_name()?.to_str()?;
    let parent = dir.parent()?.file_name()?.to_str()?;
    let valid = !profile.is_empty()
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    (matches!(parent, ".colima" | "colima") && valid).then(|| profile.to_string())
}

fn not_running(
    product: Product,
    probe: &dyn HostProbe,
    podman: &PodmanProbe,
    stale: bool,
) -> RuntimeState {
    let app = |bundle: &str, hint: &str| {
        let installed = probe.path_kind(Path::new(bundle)) == PathKind::Directory;
        (installed || stale).then(|| RuntimeState::InstalledNotRunning {
            hint: hint.to_string(),
        })
    };
    let state = match product {
        Product::OrbStack => app(
            "/Applications/OrbStack.app",
            "OrbStack is installed but not running; open it yourself (Hutan never launches it)",
        ),
        Product::DockerDesktop => app(
            "/Applications/Docker.app",
            "Docker Desktop is installed but not running; open it yourself (Hutan never launches it)",
        ),
        Product::RancherDesktop => app(
            "/Applications/Rancher Desktop.app",
            "Rancher Desktop is installed but not running; open it yourself (Hutan never launches it)",
        ),
        Product::Colima => (probe.find_executable("colima").is_some() || stale)
            .then(|| RuntimeState::InstalledNotRunning { hint: "Colima is installed but not running; run `colima start`".to_string() }),
        Product::Podman => podman.cli.as_ref().map(|_| {
            let hint = match podman.machines.as_slice() {
                [] => "no Podman machine exists; run `podman machine init` and `podman machine start`",
                machines if machines.iter().any(|m| m.running) => {
                    "a Podman machine is running but its API socket does not answer; restart it with `podman machine stop` and `podman machine start`"
                }
                _ => "the Podman machine is stopped; run `podman machine start`",
            };
            RuntimeState::InstalledNotRunning { hint: hint.to_string() }
        }),
        Product::Other => None,
    };
    state.unwrap_or(RuntimeState::Missing)
}

struct PodmanProbe {
    cli: Option<PathBuf>,
    machines: Vec<PodmanMachine>,
}

fn probe_podman(probe: &dyn HostProbe) -> PodmanProbe {
    let cli = probe.find_executable("podman");
    let machines = match &cli {
        Some(cli) => match probe.run(cli, &["machine", "inspect"]) {
            Ok(out) => parse_machines(&out.stdout),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    PodmanProbe { cli, machines }
}

/// One report per listed product (Running ones may repeat per endpoint, e.g. two Colima
/// profiles) plus one per unrecognised engine, in `RuntimeKind::listed()` order.
pub fn detect_docker_family(probe: &dyn HostProbe) -> Vec<RuntimeReport> {
    let now = probe.now_ms();
    let podman = probe_podman(probe);
    let docker_cli = probe.find_executable("docker");

    let mut running: Vec<(Product, RuntimeReport)> = Vec::new();
    let mut stale: Vec<Product> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for socket in socket_candidates(probe, &podman.machines) {
        let canonical = probe
            .canonicalize(&socket)
            .unwrap_or_else(|| socket.clone());
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical.clone());
        match probe_endpoint(probe, &socket) {
            Ok(facts) => {
                let product = identify(&canonical, &facts);
                let mut report = RuntimeReport::new(
                    RuntimeKind::DockerCompatible(product),
                    RuntimeState::Running,
                    now,
                );
                if product == Product::Colima
                    && let Some(profile) = colima_profile(&socket)
                    && profile != "default"
                {
                    report.id = RuntimeId(format!("colima-{profile}"));
                }
                report.version = engine_version(&facts);
                report.endpoint = Some(socket);
                let wants = if product == Product::Podman {
                    podman.cli.clone().or_else(|| docker_cli.clone())
                } else {
                    docker_cli.clone()
                };
                report.cli = wants;
                if report.cli.is_none() {
                    report.state = RuntimeState::Unsupported {
                        reason: format!(
                            "{} is running but no `{}` CLI was found",
                            product.display_name(),
                            if product == Product::Podman {
                                "podman"
                            } else {
                                "docker"
                            }
                        ),
                    };
                } else if let Some(os) = facts.version.as_ref().and_then(|v| v.os.as_deref())
                    && os != "linux"
                {
                    report.state = RuntimeState::Unsupported {
                        reason: format!("the engine runs {os} containers; Hutan needs Linux"),
                    };
                }
                running.push((product, report));
            }
            Err(EndpointError::Stale) => {
                stale.extend(product_from_path(&canonical).or_else(|| product_from_path(&socket)))
            }
            Err(EndpointError::NotAnEngine(_)) => {}
        }
    }

    let mut reports = Vec::new();
    for product in Product::LISTED {
        let mut found: Vec<RuntimeReport> = running
            .iter()
            .filter(|(p, _)| *p == product)
            .map(|(_, r)| r.clone())
            .collect();
        if found.is_empty() {
            let state = not_running(product, probe, &podman, stale.contains(&product));
            let mut report = RuntimeReport::new(RuntimeKind::DockerCompatible(product), state, now);
            if !matches!(report.state, RuntimeState::Missing) {
                report.cli = if product == Product::Podman {
                    podman.cli.clone()
                } else {
                    docker_cli.clone()
                };
            }
            found.push(report);
        }
        reports.extend(found);
    }
    reports.extend(
        running
            .into_iter()
            .filter(|(p, _)| *p == Product::Other)
            .map(|(_, r)| r),
    );
    reports
}
