//! `hutan host list|add|remove|test|forward` and the resolution of `--host NAME` (ADR 0022,
//! slice R-2). A host is a `[[host]]` of the user's `settings.toml`: a name and what `ssh` is
//! given. Nothing here stores or asks for a password or key.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hutan_client::{Client, ConnectOptions, Endpoint, SshHost, default_socket_path};
use hutan_protocol::PROTOCOL_VERSION;
use hutan_protocol::messages::{ClientInfo, ClientKind, DaemonStatusInfo, Empty, Request};
use hutan_settings::hosts::{self, HostProfile};
use hutan_settings::{ExperimentalSettings, SettingsError, SettingsPaths};
use serde::Serialize;
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::sleep;

use super::{format_uptime, print_line};
use crate::args::HostCommand;
use crate::error::CliError;

/// How long the daemon of a host may take to answer `hutan daemon start` on it.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(60);
/// Round trips `host test` measures.
const TEST_CALLS: usize = 5;
/// The first wait before `forward` restarts an `ssh` that ended, doubling up to the cap.
const RESTART_MIN: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(30);
/// An `ssh` that stayed up this long made the next failure a new one, not a repeat.
const STABLE: Duration = Duration::from_secs(60);
/// Failures in a row after which `forward` gives up: something permanent is wrong.
const GIVE_UP: u32 = 5;
/// The longest Unix socket path (macOS `sun_path` is 104 bytes including the NUL).
const SOCKET_PATH_MAX: usize = 100;

/// The one-line disclaimer, as `hutan host add` and `hutan host test` end with it.
pub(super) const DISCLAIMER: &str = hutan_settings::experimental::REMOTE_HOSTS_DISCLAIMER;

/// Refuses (usage error, exit 2) while `experimental.remote_hosts` is off, naming the setting.
/// Remote hosts ship switched off: they may change or break between releases and give anyone
/// with SSH access to the host the control of its user (`docs/remote.md`).
pub(super) fn require_enabled() -> Result<(), CliError> {
    let enabled = ExperimentalSettings::load(&paths()?)
        .map_err(settings_error)?
        .remote_hosts;
    if enabled {
        return Ok(());
    }
    Err(CliError::Usage(format!(
        "remote hosts are off. {DISCLAIMER}\n\
         Turn them on in Settings ▸ Experimental (\"Remote hosts over SSH\"), with `hutan ui set \
         experimental.remote_hosts true` while Hutan is open, or by adding `[experimental]` \
         `remote_hosts = true` to settings.toml. See docs/remote.md."
    )))
}

fn settings_error(error: SettingsError) -> CliError {
    CliError::Usage(error.to_string())
}

fn paths() -> Result<SettingsPaths, CliError> {
    SettingsPaths::from_env().map_err(settings_error)
}

fn ssh_host(profile: HostProfile) -> Result<SshHost, CliError> {
    SshHost::new(profile.name, profile.ssh, profile.hutan)
        .map_err(|e| CliError::Usage(e.to_string()))
}

/// The host a `--host NAME` names.
pub(super) fn resolve(name: &str) -> Result<SshHost, CliError> {
    ssh_host(hosts::find(&paths()?, name).map_err(settings_error)?)
}

pub(super) async fn run(json: bool, command: HostCommand) -> Result<(), CliError> {
    require_enabled()?;
    match command {
        HostCommand::List => list(json),
        HostCommand::Add { name, ssh, hutan } => add(json, HostProfile { name, ssh, hutan }),
        HostCommand::Remove { name } => remove(json, &name),
        HostCommand::Test { name } => test(json, &name).await,
        HostCommand::Forward {
            name,
            listen,
            remote_socket,
        } => forward(json, &name, listen, remote_socket).await,
    }
}

fn output<T: Serialize>(
    json: bool,
    value: &T,
    human: impl FnOnce() -> String,
) -> Result<(), CliError> {
    if json {
        print_line(
            &serde_json::to_string(value)
                .map_err(|e| CliError::Failed(format!("encoding output: {e}")))?,
        )
    } else {
        print_line(&human())
    }
}

fn list(json: bool) -> Result<(), CliError> {
    let profiles = hosts::list(&paths()?).map_err(settings_error)?;
    output(json, &serde_json::json!({ "hosts": profiles }), || {
        if profiles.is_empty() {
            return "no hosts; add one with `hutan host add NAME USER@HOST`".to_owned();
        }
        let width = profiles
            .iter()
            .map(|p| p.name.len())
            .max()
            .unwrap_or(0)
            .max(4);
        let ssh = profiles
            .iter()
            .map(|p| p.ssh.len())
            .max()
            .unwrap_or(0)
            .max(3);
        let mut out = format!("{:<width$}  {:<ssh$}  HUTAN", "NAME", "SSH");
        for profile in &profiles {
            out.push_str(&format!(
                "\n{:<width$}  {:<ssh$}  {}",
                profile.name,
                profile.ssh,
                profile.hutan.as_deref().unwrap_or("(PATH on the host)")
            ));
        }
        out
    })
}

fn add(json: bool, profile: HostProfile) -> Result<(), CliError> {
    hosts::add(&paths()?, &profile).map_err(settings_error)?;
    output(
        json,
        &serde_json::json!({ "added": profile, "disclaimer": DISCLAIMER }),
        || {
            format!(
                "added host {name}; try `hutan host test {name}`, then `hutan --host {name} ls`\n{DISCLAIMER}",
                name = profile.name
            )
        },
    )
}

fn remove(json: bool, name: &str) -> Result<(), CliError> {
    let removed = hosts::remove(&paths()?, name).map_err(settings_error)?;
    if !removed {
        return Err(CliError::Failed(format!("no host named `{name}`")));
    }
    output(json, &serde_json::json!({ "removed": name }), || {
        format!("removed host {name}")
    })
}

#[derive(Serialize)]
struct TestOut {
    host: String,
    ssh: String,
    connected: bool,
    /// From starting `ssh` to the daemon's welcome: SSH's own connection setup, the proxy
    /// reaching (or starting) the daemon, and the handshake.
    connect_ms: f64,
    /// Median of `daemon.status` calls over the open connection.
    round_trip_ms: f64,
    calls: usize,
    /// The protocol this CLI speaks; the host's is in `daemon`.
    protocol: String,
    daemon: DaemonStatusInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    disclaimer: &'static str,
}

fn ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1e6).round() / 1e3
}

async fn test(json: bool, name: &str) -> Result<(), CliError> {
    let host = resolve(name)?;
    let fail = |e| CliError::from_host(e, &host.name);
    let started = Instant::now();
    let client = Client::connect(ConnectOptions {
        endpoint: Some(Endpoint::Ssh(host.clone())),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Cli,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            origin: None,
        },
    })
    .await
    .map_err(fail)?;
    let connect = started.elapsed();
    let mut daemon: Option<DaemonStatusInfo> = None;
    let mut trips = Vec::with_capacity(TEST_CALLS);
    for _ in 0..TEST_CALLS {
        let call = Instant::now();
        let info: DaemonStatusInfo = client
            .call(Request::DaemonStatus(Empty {}))
            .await
            .map_err(fail)?;
        trips.push(call.elapsed());
        daemon = Some(info);
    }
    trips.sort();
    let Some(daemon) = daemon else {
        return Err(CliError::Failed("no answer to daemon.status".to_owned()));
    };
    let ours = PROTOCOL_VERSION;
    let note = (daemon.protocol != ours).then(|| {
        format!(
            "the host speaks protocol {}.{}, this CLI {}.{}: requests newer than the host's are \
             refused until one of them is updated",
            daemon.protocol.major, daemon.protocol.minor, ours.major, ours.minor
        )
    });
    let out = TestOut {
        host: host.name.clone(),
        ssh: host.destination().to_owned(),
        connected: true,
        connect_ms: ms(connect),
        round_trip_ms: ms(trips[trips.len() / 2]),
        calls: trips.len(),
        protocol: format!("{}.{}", ours.major, ours.minor),
        daemon,
        note,
        disclaimer: DISCLAIMER,
    };
    output(json, &out, || {
        let d = &out.daemon;
        let mut text = format!(
            "host {} (ssh {}): ok\n  connected and handshaken in {} ms\n  daemon.status round \
             trip {} ms (median of {})\n  hutand {} (protocol {}.{}) pid {}, up {}, {} session(s), {} client(s)",
            out.host,
            out.ssh,
            out.connect_ms,
            out.round_trip_ms,
            out.calls,
            d.daemon_version,
            d.protocol.major,
            d.protocol.minor,
            d.pid,
            format_uptime(d.uptime_ms / 1000),
            d.sessions,
            d.clients,
        );
        if let Some(note) = &out.note {
            text.push_str(&format!("\n  note: {note}"));
        }
        text.push_str(&format!("\n{DISCLAIMER}"));
        text
    })
}

/// Where `forward` listens by default: `hosts/NAME.sock` next to the local daemon's socket.
fn default_listen(name: &str) -> Result<PathBuf, CliError> {
    let socket = default_socket_path()
        .ok_or_else(|| CliError::Usage("cannot determine the Hutan home; set HUTAN_HOME".into()))?;
    let dir = socket
        .parent()
        .map(|parent| parent.join("hosts"))
        .ok_or_else(|| CliError::Usage("the daemon socket has no directory".into()))?;
    Ok(dir.join(format!("{name}.sock")))
}

/// The daemon's socket on the host, from the host's own `hutan daemon start --json` (which also
/// makes sure a daemon runs there, so the forward has something to reach).
async fn discover_remote_socket(host: &SshHost) -> Result<String, CliError> {
    let text = host
        .run(&host.hutan_command("daemon start --json"), DISCOVER_TIMEOUT)
        .await
        .map_err(|e| CliError::from_host(e, &host.name))?;
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|e| {
        CliError::Failed(format!(
            "`hutan daemon start --json` on host `{}` did not print JSON ({e}); pass \
             --remote-socket PATH",
            host.name
        ))
    })?;
    value
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            CliError::Failed(format!(
                "host `{}` did not say where its daemon listens; pass --remote-socket PATH",
                host.name
            ))
        })
}

fn prepare_listen(local: &Path) -> Result<(), CliError> {
    if local.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(CliError::Usage(format!(
            "`{}` is too long for a Unix socket; pass --listen with a shorter path",
            local.display()
        )));
    }
    if let Some(dir) = local.parent() {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    Ok(())
}

async fn forward(
    json: bool,
    name: &str,
    listen: Option<PathBuf>,
    remote_socket: Option<String>,
) -> Result<(), CliError> {
    let host = resolve(name)?;
    let local = match listen {
        Some(path) => std::path::absolute(path)?,
        None => default_listen(name)?,
    };
    prepare_listen(&local)?;
    let remote = match remote_socket {
        Some(path) => path,
        None => discover_remote_socket(&host).await?,
    };
    output(
        json,
        &serde_json::json!({ "host": name, "socket": local, "remote_socket": remote }),
        || {
            format!(
                "forwarding host {name}: {} -> {remote} (use HUTAN_SOCKET={} hutan ...); \
                 Ctrl-C stops",
                local.display(),
                local.display()
            )
        },
    )?;

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut wait = RESTART_MIN;
    let mut failures = 0u32;
    let result = loop {
        let mut child = host
            .forward(&local, &remote)
            .map_err(|e| CliError::from_host(e, name))?;
        let up = Instant::now();
        let status = tokio::select! {
            status = child.wait() => status?,
            _ = interrupt.recv() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
        };
        if up.elapsed() >= STABLE {
            failures = 0;
            wait = RESTART_MIN;
        }
        failures += 1;
        if failures >= GIVE_UP {
            break Err(CliError::Unreachable(format!(
                "ssh to host `{name}` ended {failures} times in a row (last: {status}); \
                 see its messages above"
            )));
        }
        eprintln!(
            "hutan: ssh to host `{name}` ended ({status}); forwarding again in {} s",
            wait.as_secs()
        );
        tokio::select! {
            () = sleep(wait) => {}
            _ = interrupt.recv() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
        }
        wait = (wait * 2).min(RESTART_MAX);
    };
    // `ssh` is killed when its `Child` drops (above); the socket it made is ours to remove.
    let _ = std::fs::remove_file(&local);
    result
}
