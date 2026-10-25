//! A runtime's CLI as a launcher: program, endpoint environment, and the argv builders bound
//! to one dialect.

use std::path::PathBuf;
use std::time::Duration;

use crate::argv;
use crate::kind::Dialect;
use crate::spec::{
    ContainerId, ContainerSpec, ExecIo, ExecSpec, ImageRef, Labels, SpecError, VolumeName,
};

/// A process to start. Same shape as `hutan_agents::jsonrpc::Launch` (program, args, cwd,
/// env) so the daemon can hand it to the terminal and stdio seams; kept local because this
/// crate depends on no Hutan crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// One installed CLI: which dialect, which binary, and (Docker family) which engine socket it
/// must talk to. The endpoint is passed as `DOCKER_HOST` / `CONTAINER_HOST` so Hutan drives the
/// runtime it detected, not whatever `docker context` the user last selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub dialect: Dialect,
    pub program: PathBuf,
    pub endpoint: Option<PathBuf>,
}

impl Cli {
    pub fn new(dialect: Dialect, program: impl Into<PathBuf>) -> Self {
        Self {
            dialect,
            program: program.into(),
            endpoint: None,
        }
    }

    #[must_use]
    pub fn with_endpoint(mut self, socket: impl Into<PathBuf>) -> Self {
        self.endpoint = Some(socket.into());
        self
    }

    fn launch(&self, args: Vec<String>) -> Launch {
        let mut env = Vec::new();
        if let Some(socket) = &self.endpoint {
            let key = match self.dialect {
                Dialect::Apple => None,
                Dialect::Docker => Some("DOCKER_HOST"),
                Dialect::Podman => Some("CONTAINER_HOST"),
            };
            if let Some(key) = key {
                env.push((key.to_string(), format!("unix://{}", socket.display())));
            }
        }
        Launch {
            program: self.program.clone(),
            args,
            cwd: None,
            env,
        }
    }

    pub fn create(&self, spec: &ContainerSpec) -> Result<Launch, SpecError> {
        argv::create_args(self.dialect, spec).map(|a| self.launch(a))
    }

    pub fn start(&self, id: &ContainerId) -> Launch {
        self.launch(argv::start_args(id))
    }

    pub fn stop(&self, id: &ContainerId, grace: Duration) -> Launch {
        self.launch(argv::stop_args(id, grace))
    }

    pub fn remove(&self, id: &ContainerId, force: bool) -> Launch {
        self.launch(argv::remove_args(self.dialect, id, force))
    }

    pub fn inspect(&self, id: &ContainerId) -> Launch {
        self.launch(argv::inspect_args(self.dialect, id))
    }

    pub fn list(&self) -> Launch {
        self.launch(argv::list_args(self.dialect))
    }

    pub fn image_inspect(&self, image: &ImageRef) -> Launch {
        self.launch(argv::image_inspect_args(image))
    }

    pub fn volume_create(&self, name: &VolumeName, labels: &Labels) -> Launch {
        self.launch(argv::volume_create_args(name, labels))
    }

    pub fn volume_inspect(&self, name: &VolumeName) -> Launch {
        self.launch(argv::volume_inspect_args(name))
    }

    /// `exec -it …` for the daemon's PTY (terminals, interactive logins).
    pub fn exec_pty(&self, id: &ContainerId, cmd: &ExecSpec) -> Launch {
        self.launch(argv::exec_args(self.dialect, id, cmd, ExecIo::Pty))
    }

    /// `exec -i …` for stdio peers (ACP, `codex app-server`, the relay).
    pub fn exec_stdio(&self, id: &ContainerId, cmd: &ExecSpec) -> Launch {
        self.launch(argv::exec_args(self.dialect, id, cmd, ExecIo::Stdio))
    }
}
