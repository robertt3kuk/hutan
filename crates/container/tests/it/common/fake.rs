//! A scripted [`HostProbe`]: the machine as a table of recorded answers.
//!
//! It also enforces the read-only contract: a command or request outside the allow-list below
//! panics the test, and every call that was made is recorded in `log`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hutan_container::probe::parse_http_response;
use hutan_container::{CommandOutput, HostProbe, HttpResponse, PathKind, ProbeError};

/// Recorded vendor output; sources are listed in `tests/fixtures/SOURCES.md`.
pub mod fixtures {
    // Apple `container`: SystemStatus.swift payloads and the command reference's `system version`.
    pub const APPLE_STATUS_RUNNING: &str = include_str!("../../fixtures/apple/status_running.json");
    pub const APPLE_STATUS_NOT_RUNNING: &str =
        include_str!("../../fixtures/apple/status_not_running.json");
    pub const APPLE_STATUS_UNREGISTERED: &str =
        include_str!("../../fixtures/apple/status_unregistered.json");
    pub const APPLE_STATUS_UNKNOWN: &str =
        include_str!("../../fixtures/apple/status_unknown_state.json");
    pub const APPLE_VERSION_WITH_SERVER: &str =
        include_str!("../../fixtures/apple/system_version_with_server.json");
    pub const APPLE_VERSION_CLIENT_ONLY: &str =
        include_str!("../../fixtures/apple/system_version_client_only_1_6_1.json");

    // Docker Engine API, per product (real listings; Rancher and Lima are synthetic).
    pub const DOCKER_DESKTOP: Engine = Engine {
        ping: include_str!("../../fixtures/docker/docker_desktop/ping.http"),
        version: Some(include_str!(
            "../../fixtures/docker/docker_desktop/version.json"
        )),
        info: Some(include_str!(
            "../../fixtures/docker/docker_desktop/info.json"
        )),
    };
    pub const COLIMA: Engine = Engine {
        ping: include_str!("../../fixtures/docker/colima/ping.http"),
        version: Some(include_str!("../../fixtures/docker/colima/version.json")),
        info: Some(include_str!("../../fixtures/docker/colima/info.json")),
    };
    pub const ORBSTACK: Engine = Engine {
        ping: include_str!("../../fixtures/docker/orbstack/ping.http"),
        version: Some(include_str!("../../fixtures/docker/orbstack/version.json")),
        info: Some(include_str!("../../fixtures/docker/orbstack/info.json")),
    };
    pub const RANCHER_DESKTOP: Engine = Engine {
        ping: include_str!("../../fixtures/docker/rancher_desktop/ping.http"),
        version: Some(include_str!(
            "../../fixtures/docker/rancher_desktop/version.json"
        )),
        info: Some(include_str!(
            "../../fixtures/docker/rancher_desktop/info.json"
        )),
    };
    pub const LIMA: Engine = Engine {
        ping: include_str!("../../fixtures/docker/lima/ping.http"),
        version: Some(include_str!("../../fixtures/docker/lima/version.json")),
        info: Some(include_str!("../../fixtures/docker/lima/info.json")),
    };
    pub const NOT_AN_ENGINE: Engine = Engine {
        ping: include_str!("../../fixtures/docker/not_an_engine/ping.http"),
        version: None,
        info: None,
    };

    // Podman: the compat API has no `/info` fixture; detection must not need it.
    pub const PODMAN: Engine = Engine {
        ping: include_str!("../../fixtures/podman/ping.http"),
        version: Some(include_str!("../../fixtures/podman/version.json")),
        info: None,
    };
    pub const PODMAN_MACHINE_RUNNING: &str =
        include_str!("../../fixtures/podman/machine_inspect_running.json");
    pub const PODMAN_MACHINE_STOPPED: &str =
        include_str!("../../fixtures/podman/machine_inspect_stopped.json");

    /// What one engine socket answers.
    #[derive(Clone, Copy)]
    pub struct Engine {
        pub ping: &'static str,
        pub version: Option<&'static str>,
        pub info: Option<&'static str>,
    }
}

pub const HOME: &str = "/Users/me";
/// The socket path in `podman-machine-inspect(1)`.
pub const PODMAN_SOCKET: &str =
    "/var/folders/9r/n3056v597wv2cq8s2j80bdnw0000gn/T/podman/podman-machine-default-api.sock";

const ALLOWED_GETS: [&str; 3] = ["/_ping", "/version", "/info"];

type Answer<T> = Result<T, ProbeError>;

#[derive(Default)]
pub struct FakeProbe {
    env: BTreeMap<String, String>,
    exes: BTreeMap<String, PathBuf>,
    kinds: BTreeMap<PathBuf, PathKind>,
    canonical: BTreeMap<PathBuf, PathBuf>,
    dirs: BTreeMap<PathBuf, Vec<PathBuf>>,
    commands: BTreeMap<String, Answer<CommandOutput>>,
    http: BTreeMap<(PathBuf, String), Answer<HttpResponse>>,
    /// Every command and request made, in order.
    pub log: RefCell<Vec<String>>,
}

pub fn output(status: i32, stdout: &str) -> Answer<CommandOutput> {
    Ok(CommandOutput {
        status: Some(status),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

fn json_ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        body: body.to_string(),
    }
}

impl FakeProbe {
    /// A Mac with a home directory and nothing installed.
    pub fn mac() -> Self {
        let mut probe = Self::default();
        probe.env.insert("HOME".to_string(), HOME.to_string());
        probe
    }

    pub fn exe(mut self, name: &str, path: &str) -> Self {
        self.exes.insert(name.to_string(), PathBuf::from(path));
        self
    }

    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.insert(name.to_string(), value.to_string());
        self
    }

    pub fn dir(mut self, path: &str) -> Self {
        self.kinds.insert(PathBuf::from(path), PathKind::Directory);
        self
    }

    pub fn listing(mut self, dir: &str, entries: &[&str]) -> Self {
        self.dirs.insert(
            PathBuf::from(dir),
            entries.iter().map(PathBuf::from).collect(),
        );
        self
    }

    pub fn symlink(mut self, link: &str, target: &str) -> Self {
        self.canonical
            .insert(PathBuf::from(link), PathBuf::from(target));
        self
    }

    /// `sw_vers -productVersion` prints `version`.
    pub fn macos(self, version: &str) -> Self {
        self.command(
            "/usr/bin/sw_vers -productVersion",
            output(0, &format!("{version}\n")),
        )
    }

    pub fn command(mut self, line: &str, answer: Answer<CommandOutput>) -> Self {
        self.commands.insert(line.to_string(), answer);
        self
    }

    /// A socket file nothing listens on.
    pub fn stale_socket(mut self, path: &str) -> Self {
        self.kinds.insert(PathBuf::from(path), PathKind::Socket);
        self.http.insert(
            (PathBuf::from(path), "/_ping".to_string()),
            Err(ProbeError::Refused),
        );
        self
    }

    /// A socket answering with a recorded engine.
    pub fn engine(mut self, path: &str, engine: fixtures::Engine) -> Self {
        let path = PathBuf::from(path);
        self.kinds.insert(path.clone(), PathKind::Socket);
        let ping = parse_http_response(engine.ping.replace('\n', "\r\n").as_bytes())
            .expect("fixture ping parses");
        self.http
            .insert((path.clone(), "/_ping".to_string()), Ok(ping));
        if let Some(body) = engine.version {
            self.http
                .insert((path.clone(), "/version".to_string()), Ok(json_ok(body)));
        }
        if let Some(body) = engine.info {
            self.http
                .insert((path, "/info".to_string()), Ok(json_ok(body)));
        }
        self
    }

    /// Commands and requests made, for assertions.
    pub fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

fn is_read_only_command(line: &str) -> bool {
    matches!(
        line,
        "/usr/bin/sw_vers -productVersion"
            | "/usr/local/bin/container system status --format json"
            | "/usr/local/bin/container system version --format json"
            | "/opt/homebrew/bin/podman machine inspect"
            | "/usr/local/bin/podman machine inspect"
    )
}

impl HostProbe for FakeProbe {
    fn now_ms(&self) -> u64 {
        1_790_000_000_000
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.env.get("HOME").map(PathBuf::from)
    }

    fn env_var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn find_executable(&self, name: &str) -> Option<PathBuf> {
        self.exes.get(name).cloned()
    }

    fn path_kind(&self, path: &Path) -> PathKind {
        let resolved = self.canonical.get(path).map_or(path, |p| p.as_path());
        self.kinds
            .get(resolved)
            .or_else(|| self.kinds.get(path))
            .copied()
            .unwrap_or(PathKind::Missing)
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        match self.canonical.get(path) {
            Some(target) => Some(target.clone()),
            None => (self.path_kind(path) != PathKind::Missing).then(|| path.to_path_buf()),
        }
    }

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        self.dirs.get(dir).cloned().unwrap_or_default()
    }

    fn run(&self, program: &Path, args: &[&str]) -> Answer<CommandOutput> {
        let line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().map(|a| (*a).to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        self.log.borrow_mut().push(line.clone());
        assert!(
            is_read_only_command(&line),
            "detection ran a command outside the read-only allow-list: {line}"
        );
        self.commands
            .get(&line)
            .cloned()
            .unwrap_or(Err(ProbeError::NotFound))
    }

    fn http_get(&self, socket: &Path, path: &str) -> Answer<HttpResponse> {
        self.log
            .borrow_mut()
            .push(format!("GET {} {path}", socket.display()));
        assert!(ALLOWED_GETS.contains(&path), "detection requested {path}");
        self.http
            .get(&(socket.to_path_buf(), path.to_string()))
            .cloned()
            .unwrap_or(Err(ProbeError::NotFound))
    }
}
