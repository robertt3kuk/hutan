//! Shared harness for the session and attach tests: the real `hutan` binary against a real
//! `hutand` (in-process on a temporary `HUTAN_HOME`, or a spawned process).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hutan_daemon::{DaemonConfig, Paths, RunningDaemon};
use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

pub const HUTAN: &str = env!("CARGO_BIN_EXE_hutan");
pub const WAIT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", self.stdout))
    }

    /// Asserts success and returns `self`.
    pub fn ok(self) -> Self {
        assert_eq!(self.code, 0, "{self:?}");
        self
    }
}

pub struct TestDaemon {
    pub _dir: TempDir,
    pub paths: Paths,
    pub daemon: RunningDaemon,
}

impl TestDaemon {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// [`start`](Self::start) with `tweak` applied to the daemon's config.
    pub async fn start_with(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        Self::start_toml("", tweak).await
    }

    /// [`start_with`](Self::start_with) and `toml` as `daemon.toml` (written first; it may
    /// name files of `dir`, the temporary `HUTAN_HOME`, through the closure's argument).
    pub async fn start_toml(toml: &str, tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        if !toml.is_empty() {
            std::fs::write(&paths.config_file, toml).unwrap();
        }
        // Sessions find this build of the CLI through `HUTAN_BIN`, as in a real install.
        let mut config = DaemonConfig {
            hutan_bin: Some(PathBuf::from(HUTAN)),
            ..DaemonConfig::new(paths.clone())
        };
        tweak(&mut config);
        let daemon = hutan_daemon::start(config).await.unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    pub async fn hutan(&self, args: &[&str]) -> Out {
        hutan(&self.paths.socket, args).await
    }

    /// `hutan new --json` of `argv`; returns the session id.
    pub async fn new_session(&self, extra: &[&str], argv: &[&str]) -> String {
        let mut args = vec!["new", "--json"];
        args.extend_from_slice(extra);
        args.push("--");
        args.extend_from_slice(argv);
        let out = self.hutan(&args).await.ok();
        out.json()["session"]["id"].as_str().unwrap().to_owned()
    }

    /// The session objects of `hutan ls --json`.
    pub async fn sessions(&self) -> Vec<Value> {
        let out = self.hutan(&["ls", "--json"]).await.ok();
        out.json()["sessions"].as_array().unwrap().clone()
    }

    pub async fn session(&self, id: &str) -> Value {
        self.sessions()
            .await
            .into_iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("session {id} not listed"))
    }

    /// Polls `hutan ls` until the session's `state.state` is `state`.
    pub async fn wait_state(&self, id: &str, state: &str) -> Value {
        let result = timeout(WAIT, async {
            loop {
                let session = self.session(id).await;
                if session["state"]["state"] == state {
                    return session;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        result.unwrap_or_else(|_| panic!("session {id} never reached state {state}"))
    }

    pub async fn stop(self) {
        self.daemon.shutdown();
        timeout(WAIT, self.daemon.wait()).await.unwrap().unwrap();
    }
}

/// Environment-free invocation: `HUTAN_*` from the parent never leak in. `--socket` goes
/// first so it cannot end up inside a trailing `-- ARGV`.
pub fn command(binary: &Path, socket: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    command
        .args(args)
        .env_remove("HUTAN_HOME")
        .env_remove("HUTAN_SOCKET")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Switches remote hosts on in the `settings.toml` of `home` (they ship off, R-2).
pub fn enable_remote_hosts(home: &Path) {
    std::fs::write(
        home.join("settings.toml"),
        "[experimental]\nremote_hosts = true\n",
    )
    .unwrap();
}

/// `hutan args…` with `HUTAN_HOME` = `home` (where `settings.toml` with the `[[host]]` profiles
/// lives) and `HUTAN_SSH` = `ssh`, the program `--host` runs instead of the system `ssh`.
pub async fn hutan_in(home: &Path, ssh: &Path, args: &[&str]) -> Out {
    let mut command = command(Path::new(HUTAN), None, args);
    command.env("HUTAN_HOME", home).env("HUTAN_SSH", ssh);
    finish(command).await
}

/// Held while a child is spawned. On macOS std creates a child's stdio pipes with `pipe()` and
/// sets `FD_CLOEXEC` afterwards, so a child spawned at that moment by another task inherits
/// both ends of the other child's pipe. A long-lived child (a CLI waiting for its approval) then
/// keeps a short one's output open and `wait_with_output` never sees its end. One spawn at a
/// time in the test process closes that window.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Spawns `command`, never at the same time as another child of these tests.
pub fn spawn_child(command: &mut Command) -> tokio::process::Child {
    let _one_at_a_time = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    command.spawn().unwrap()
}

pub async fn finish(mut command: Command) -> Out {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = spawn_child(&mut command);
    let output = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    Out {
        code: output.status.code().expect("killed by a signal"),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

pub async fn hutan(socket: &Path, args: &[&str]) -> Out {
    finish(command(Path::new(HUTAN), Some(socket), args)).await
}

/// Directory holding `hutand`, next to the `hutan` test binary (same profile). Tests never
/// build it: `just test` builds the workspace binaries first.
pub fn hutand_dir() -> PathBuf {
    let dir = Path::new(HUTAN).parent().unwrap().to_owned();
    assert!(
        dir.join("hutand").is_file(),
        "hutand not built: run `cargo build -p hutan-daemon --bin hutand` (or `just test`)"
    );
    dir
}

/// The fake ACP agent, next to the `hutan` test binary; `just test` builds the workspace
/// binaries first.
pub fn acp_fixture_bin() -> PathBuf {
    let bin = Path::new(HUTAN).parent().unwrap().join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// The fake codex app server, next to the `hutan` test binary; `just test` builds the
/// workspace binaries first.
pub fn codex_fixture_bin() -> PathBuf {
    let bin = Path::new(HUTAN)
        .parent()
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}
