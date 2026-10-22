//! The commands behind `hutan`'s subcommands.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::identity::{self, DaemonIdentity, Mismatch};
use hutan_client::{
    Client, ClientError, ConnectOptions, DaemonFiles, StartFailure, default_socket_path,
    move_data_aside, start_failure,
};
use hutan_protocol::ErrorBody;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonBuild, DaemonShutdown, DaemonStatusInfo, Empty, Event, Request,
    SessionList, SessionListFilter, SessionSnapshot, Subscribed,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::sleep;

use crate::args::{
    AgentCommand, ApiArgs, Cli, Command, DaemonCommand, EventsArgs, HistoryArgs, HistorySubcommand,
};
use crate::error::{CliError, is_unreachable};

mod accounts;
mod agent;
mod agents;
mod assistant;
mod attach;
mod chat;
mod checkpoints;
mod config;
mod files;
mod grants;
mod help;
mod history;
mod integration;
mod lsp;
mod memory;
mod notify;
mod projects;
mod providers;
mod recovery;
mod resources;
mod sessions;
mod tasks;
mod theme;
mod ui;
mod workspaces;
mod worktrees;

const STOP_WAIT: Duration = Duration::from_secs(10);
const STOP_POLL: Duration = Duration::from_millis(25);

pub async fn run(cli: Cli) -> Result<(), CliError> {
    // Whether a socket can be found is each command's own business: the agent hooks must not
    // fail on it, the integration commands do not need one.
    let ctx = Ctx::new(&cli);
    match cli.command {
        Command::Agent(AgentCommand::Interrupt(args)) => ctx?.agent_interrupt(args).await,
        Command::Agent(command) => {
            agent::run(ctx.ok(), &command).await;
            Ok(())
        }
        Command::Integration(command) => integration::run(cli.json, command),
        Command::Theme(command) => theme::run(cli.json, command),
        Command::Help(args) => help::run(&args.topic),
        Command::Mcp => crate::mcp::run(ctx?.socket_arg).await,
        Command::Ui(command) => ctx?.ui(command).await,
        Command::Setup => ctx?.setup().await,
        Command::Daemon(DaemonCommand::Start) => ctx?.daemon_start().await,
        Command::Daemon(DaemonCommand::Status) => ctx?.daemon_status().await,
        Command::Daemon(DaemonCommand::Why) => ctx?.daemon_why().await,
        Command::Daemon(DaemonCommand::ResetData { yes }) => ctx?.daemon_reset_data(yes).await,
        Command::Daemon(DaemonCommand::Stop {
            stop_sessions,
            forget,
        }) => ctx?.daemon_stop(stop_sessions, forget).await,
        Command::Daemon(DaemonCommand::Restart { stop_sessions }) => {
            ctx?.daemon_restart(stop_sessions).await
        }
        Command::Agents(args) => ctx?.agents(args).await,
        Command::Ps(args) => ctx?.ps(args).await,
        Command::Top(args) => ctx?.top(args).await,
        Command::Kill(args) => ctx?.kill(args).await,
        Command::Ws(command) => ctx?.ws(command).await,
        Command::Project(command) => ctx?.project(command).await,
        Command::Api(args) => ctx?.api(args).await,
        Command::Events(args) => ctx?.events(args).await,
        Command::New(args) => ctx?.new_session(args).await,
        Command::Ls(args) => ctx?.ls(args).await,
        Command::Attach(args) => ctx?.attach(&args.target).await,
        Command::Send(args) => ctx?.send(args).await,
        Command::Stop(args) => ctx?.stop(&args.target).await,
        Command::Restart(args) => ctx?.restart(&args.target).await,
        Command::Close(args) => ctx?.close(&args.target, args.force).await,
        Command::Reopen(args) => ctx?.reopen(&args.target).await,
        Command::Recovery(args) => ctx?.recovery(args).await,
        Command::Rename(args) => ctx?.rename(args).await,
        Command::History(HistoryArgs {
            command: Some(HistorySubcommand::Sessions(args)),
            ..
        }) => ctx?.history_sessions(args).await,
        Command::History(args) => ctx?.history(args).await,
        Command::Search(args) => ctx?.search(args).await,
        Command::Index(command) => ctx?.index(command).await,
        Command::Chat(command) => ctx?.chat(command).await,
        Command::Checkpoint(command) => ctx?.checkpoint(command).await,
        Command::Account(command) => ctx?.account(command).await,
        Command::Provider(command) => ctx?.provider(command).await,
        Command::Config(command) => ctx?.config(command).await,
        Command::Ask(args) => ctx?.ask(args).await,
        Command::Assistant(command) => ctx?.assistant(command).await,
        Command::Grants(command) => ctx?.grants(command).await,
        Command::Memory(command) => ctx?.memory(command).await,
        Command::Fs(command) => ctx?.fs(command).await,
        Command::Lsp(args) => ctx?.lsp(args).await,
        Command::Diagnostics(args) => ctx?.diagnostics(args).await,
        Command::Open(args) => ctx?.open(args).await,
        Command::Approvals => ctx?.approvals().await,
        Command::Approve(args) => ctx?.approve(args).await,
        Command::Questions => ctx?.questions().await,
        Command::Answer(args) => ctx?.answer(args).await,
        Command::Notify(command) => ctx?.notify(command).await,
        Command::Wt(command) => ctx?.wt(command).await,
        Command::Finish(args) => ctx?.finish(args).await,
        Command::Task(command) => ctx?.task(command).await,
    }
}

struct Ctx {
    /// `--socket` as given; `None` lets the client resolve it from the environment.
    socket_arg: Option<PathBuf>,
    /// The socket that will be dialed, for messages.
    socket: PathBuf,
    json: bool,
}

impl Ctx {
    fn new(cli: &Cli) -> Result<Self, CliError> {
        let socket = cli
            .socket
            .clone()
            .or_else(default_socket_path)
            .ok_or_else(|| CliError::from_client(ClientError::NoSocketPath, Path::new("")))?;
        Ok(Self {
            socket_arg: cli.socket.clone(),
            socket,
            json: cli.json,
        })
    }

    fn options(&self, autostart: Option<PathBuf>) -> ConnectOptions {
        ConnectOptions {
            socket: self.socket_arg.clone(),
            autostart,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                origin: session_origin(),
            },
        }
    }

    fn fail(&self, error: ClientError) -> CliError {
        CliError::from_client(error, &self.socket)
    }

    async fn connect(&self) -> Result<Client, CliError> {
        Client::connect(self.options(None))
            .await
            .map_err(|e| self.fail(e))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        client: &Client,
        request: Request,
    ) -> Result<T, CliError> {
        client.call(request).await.map_err(|e| self.fail(e))
    }

    /// JSON when `--json`, otherwise the human rendering.
    fn print(
        &self,
        value: &impl Serialize,
        human: impl FnOnce() -> String,
    ) -> Result<(), CliError> {
        if self.json {
            print_line(&serde_json::to_string(value).map_err(io_error)?)
        } else {
            print_line(&human())
        }
    }

    // ---- daemon ----------------------------------------------------------------------------

    /// Connects to the daemon, starting `hutand` if nothing listens; `true` when it was started.
    async fn ensure_daemon(&self) -> Result<(Client, bool), CliError> {
        match Client::connect(self.options(None)).await {
            Ok(client) => Ok((client, false)),
            Err(ClientError::Io(e)) if is_unreachable(&e) => {
                let binary = find_hutand()?;
                let client = Client::connect(self.options(Some(binary)))
                    .await
                    .map_err(|e| self.fail(e))?;
                Ok((client, true))
            }
            Err(e) => Err(self.fail(e)),
        }
    }

    async fn start_output(&self) -> Result<StartOut, CliError> {
        let (client, started) = self.ensure_daemon().await?;
        let info: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        Ok(StartOut {
            started,
            status: StatusOut::new(&self.socket, info, &client),
        })
    }

    async fn daemon_start(&self) -> Result<(), CliError> {
        let out = self.start_output().await?;
        self.print(&out, || {
            let verb = if out.started {
                "started"
            } else {
                "already running"
            };
            format!("{verb}: {}", out.status.human())
        })
    }

    async fn daemon_status(&self) -> Result<(), CliError> {
        let client = match self.connect().await {
            Ok(client) => client,
            Err(error @ CliError::Unreachable(_)) => {
                let failure = StartFailure::recorded(&self.files());
                if self.json {
                    print_line(
                        &serde_json::json!({
                            "running": false,
                            "socket": self.socket,
                            "start_error": failure.as_ref().and_then(|f| f.record.as_ref()),
                        })
                        .to_string(),
                    )?;
                }
                return Err(match failure {
                    Some(failure) => CliError::Unreachable(format!(
                        "{error}\nThe last start failed: {}\nRun `hutan daemon why` for details.",
                        failure.sentence()
                    )),
                    None => error,
                });
            }
            Err(e) => return Err(e),
        };
        let info: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        let out = StatusOut::new(&self.socket, info, &client);
        self.print(&out, || out.human())
    }

    /// Where the daemon behind this socket keeps its start-error record and logs.
    fn files(&self) -> DaemonFiles {
        DaemonFiles::for_socket(&self.socket)
    }

    /// `hutan daemon why`: whether hutand runs, the record of its last failed start, and the
    /// ends of its start output and of its log.
    async fn daemon_why(&self) -> Result<(), CliError> {
        let running = match self.connect().await {
            Ok(client) => Some(
                self.call::<DaemonStatusInfo>(&client, Request::DaemonStatus(Empty {}))
                    .await?,
            ),
            Err(CliError::Unreachable(_)) => None,
            Err(e) => return Err(e),
        };
        let files = self.files();
        let failure = StartFailure::recorded(&files);
        let start_log = files.start_log();
        let daemon_log = files.daemon_log();
        let start_tail = start_failure::tail(&start_log, start_failure::TAIL_LINES);
        let log_tail = start_failure::tail(&daemon_log, start_failure::TAIL_LINES);
        let value = serde_json::json!({
            "running": running.is_some(),
            "pid": running.as_ref().map(|info| info.pid),
            "socket": self.socket,
            "start_error": failure.as_ref().and_then(|f| f.record.as_ref()),
            "sentence": failure.as_ref().map(StartFailure::sentence),
            "start_log": {"path": start_log, "tail": start_tail},
            "log": {"path": daemon_log, "tail": log_tail},
        });
        self.print(&value, || {
            let mut out = match &running {
                Some(info) => format!(
                    "hutand is running (pid {}) on {}.",
                    info.pid,
                    self.socket.display()
                ),
                None => format!("hutand is not running ({}).", self.socket.display()),
            };
            match &failure {
                Some(failure) => {
                    out.push_str("\n\nLast start error: ");
                    out.push_str(failure.details().trim_end());
                    if failure.schema_too_new().is_some() {
                        out.push_str(
                            "\n\nTo start with empty data instead, run `hutan daemon reset-data \
                             --yes`: it moves your data into backups/ and deletes nothing.",
                        );
                    }
                }
                None => out.push_str("\nNo start error is recorded."),
            }
            for (path, tail) in [(&start_log, &start_tail), (&daemon_log, &log_tail)] {
                match tail {
                    Some(tail) => {
                        out.push_str(&format!("\n\nEnd of {}:\n{tail}", path.display()));
                    }
                    None => out.push_str(&format!("\n\n{}: empty or missing", path.display())),
                }
            }
            out
        })
    }

    /// `hutan daemon reset-data --yes`: start with empty data. Moves the database the last
    /// failed start named (else `hutan.db` next to the socket) into `backups/`.
    async fn daemon_reset_data(&self, yes: bool) -> Result<(), CliError> {
        if !yes {
            return Err(CliError::Usage(
                "`hutan daemon reset-data` moves your Hutan database into backups/ so the next \
                 hutand starts with empty data (nothing is deleted); run it with --yes to do it"
                    .to_owned(),
            ));
        }
        if self.connect().await.is_ok() {
            return Err(CliError::Failed(
                "hutand is running; stop it first (`hutan daemon stop`)".to_owned(),
            ));
        }
        let files = self.files();
        let record = files.read_record();
        let moved = move_data_aside(&files, record.as_ref())
            .map_err(|error| CliError::Failed(error.to_string()))?;
        let value = serde_json::json!({
            "backup": moved.backup,
            "moved": moved
                .moved
                .iter()
                .map(|(from, to)| serde_json::json!({"from": from, "to": to}))
                .collect::<Vec<_>>(),
        });
        self.print(&value, || {
            format!(
                "Moved your data to {}\nThe next `hutan daemon start` starts with empty data.",
                moved.backup.display()
            )
        })
    }

    async fn daemon_stop(&self, stop_sessions: bool, forget: bool) -> Result<(), CliError> {
        let client = self.connect().await?;
        self.shutdown_daemon(client, stop_sessions, forget, false)
            .await?;
        self.print(&serde_json::json!({"stopped": true}), || {
            "hutand stopped".to_owned()
        })
    }

    /// Asks the connected daemon to shut down and waits until its socket is gone, so a new
    /// daemon can start. `restart`: one is started right after (other clients are told so).
    async fn shutdown_daemon(
        &self,
        client: Client,
        stop_sessions: bool,
        forget: bool,
        restart: bool,
    ) -> Result<(), CliError> {
        match client
            .call::<Value>(Request::DaemonShutdown(DaemonShutdown {
                stop_sessions,
                forget,
                restart,
            }))
            .await
        {
            Ok(_) | Err(ClientError::Disconnected) => {}
            Err(e) => return Err(self.fail(e)),
        }
        drop(client);
        // The daemon removes its socket just before releasing its lock, so once the file is
        // gone `daemon start` can succeed.
        let gone = tokio::time::timeout(STOP_WAIT, async {
            while std::fs::symlink_metadata(&self.socket).is_ok() {
                sleep(STOP_POLL).await;
            }
        })
        .await;
        if gone.is_err() {
            return Err(CliError::Failed(format!(
                "hutand accepted the shutdown but {} is still there after {}s",
                self.socket.display(),
                STOP_WAIT.as_secs()
            )));
        }
        Ok(())
    }

    /// Stops the daemon and starts a new one (new epoch). Refuses while sessions are live
    /// unless `stop_sessions`; a daemon that is not running is just started.
    async fn daemon_restart(&self, stop_sessions: bool) -> Result<(), CliError> {
        let was_running = match self.connect().await {
            Ok(client) => {
                if !stop_sessions {
                    let list: SessionList = self
                        .call(&client, Request::SessionList(SessionListFilter::default()))
                        .await?;
                    let live = list.sessions.iter().filter(|s| s.state.is_live()).count();
                    if live > 0 {
                        return Err(CliError::Failed(format!(
                            "{live} live session(s); hutand was not restarted. \
                             Run `hutan daemon restart --stop-sessions` to stop them and restart"
                        )));
                    }
                }
                self.shutdown_daemon(client, stop_sessions, false, true)
                    .await?;
                true
            }
            Err(CliError::Unreachable(_)) => false,
            Err(e) => return Err(e),
        };
        let out = RestartOut {
            was_running,
            start: self.start_output().await?,
        };
        self.print(&out, || format!("restarted: {}", out.start.status.human()))
    }

    // ---- raw API and events ------------------------------------------------------------------

    async fn api(&self, args: ApiArgs) -> Result<(), CliError> {
        let params = match args.params.as_deref() {
            Some(text) => serde_json::from_str(text)
                .map_err(|e| CliError::Usage(format!("params are not valid JSON: {e}")))?,
            None => Value::Object(serde_json::Map::new()),
        };
        // A request the typed protocol cannot express never reaches the wire; it gets the
        // same error body the daemon would answer with.
        let outcome = match Request::decode(&args.method, params) {
            Ok(request) => {
                let client = self.connect().await?;
                match client.call::<Value>(request).await {
                    Ok(value) => Ok(value),
                    Err(ClientError::Remote(body)) => Err(body),
                    Err(e) => return Err(self.fail(e)),
                }
            }
            Err(body) => Err(body),
        };
        match outcome {
            Ok(value) => print_line(&json_text(&value, self.json)?),
            Err(body) => {
                print_line(&json_text(&error_value(&body), self.json)?)?;
                Err(CliError::Reported)
            }
        }
    }

    async fn events(&self, args: EventsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        // Subscribe before the snapshot so nothing between them is lost; events at or below
        // the snapshot's seq are already part of it.
        let mut stream = args.follow.then(|| client.events());
        if args.follow {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let snapshot: SessionSnapshot = self
            .call(&client, Request::SessionSnapshot(Empty {}))
            .await?;
        let epoch = client.epoch();
        let line = |seq: u64, event: &Event| -> Result<String, CliError> {
            let (name, data) = event
                .encode()
                .map_err(|e| CliError::Failed(e.to_string()))?;
            serde_json::to_string(&serde_json::json!({
                "epoch": epoch,
                "seq": seq,
                "event": name,
                "data": data,
            }))
            .map_err(io_error)
        };
        for workspace in snapshot.workspaces {
            print_line(&line(
                snapshot.seq.0,
                &Event::WorkspaceUpdated { workspace },
            )?)?;
        }
        for session in snapshot.sessions {
            print_line(&line(snapshot.seq.0, &Event::SessionUpdated { session })?)?;
        }
        let Some(stream) = stream.as_mut() else {
            return Ok(());
        };
        loop {
            match stream.recv().await {
                Ok((seq, event)) if seq > snapshot.seq => print_line(&line(seq.0, &event)?)?,
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    return Err(CliError::Failed(format!(
                        "fell behind the event stream by {missed} events; rerun to resync"
                    )));
                }
                Err(RecvError::Closed) => return Err(self.fail(ClientError::Disconnected)),
            }
        }
    }
}

#[derive(Serialize)]
struct StatusOut {
    running: bool,
    socket: PathBuf,
    #[serde(flatten)]
    info: DaemonStatusInfo,
    /// Which build answers (protocol 0.42; absent from older daemons).
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<DaemonBuild>,
    /// How it differs from the `hutand` this CLI starts; absent when it does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    mismatch: Option<MismatchOut>,
}

#[derive(Serialize)]
struct MismatchOut {
    /// `foreign`: another executable; `different`: another build of it; `newer`: a newer
    /// build than this CLI.
    kind: &'static str,
    /// What this CLI would start: its version, commit and `hutand`.
    cli_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_git_sha: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_hutand: Option<PathBuf>,
    note: String,
}

impl MismatchOut {
    /// How the daemon behind `client` differs from the `hutand` beside this CLI.
    fn of(client: &Client) -> Option<Self> {
        let hutand = find_hutand().ok();
        let own = DaemonIdentity::own(hutand.as_deref());
        let running = DaemonIdentity::of(client.welcome());
        let restart = "`hutan daemon restart` starts the one beside this CLI";
        let (kind, note) = match identity::compare(&own, &running)? {
            Mismatch::Foreign { path } => (
                "foreign",
                format!(
                    "this is not the hutand beside this CLI ({}) but {}; {restart}",
                    own.exe_path
                        .as_deref()
                        .map_or_else(|| "none".into(), |p| p.display().to_string()),
                    path.display()
                ),
            ),
            Mismatch::Different => (
                "different",
                format!(
                    "this CLI is Hutan {}, the daemon runs {}; {restart}",
                    own.build_text(),
                    running.build_text()
                ),
            ),
            Mismatch::Newer => (
                "newer",
                format!(
                    "the daemon runs Hutan {}, newer than this CLI ({}); update Hutan: `{}`",
                    running.build_text(),
                    own.build_text(),
                    start_failure::UPDATE_COMMAND
                ),
            ),
        };
        Some(Self {
            kind,
            cli_version: identity::OWN_VERSION,
            cli_git_sha: identity::own_git_sha(),
            cli_hutand: own.exe_path,
            note,
        })
    }
}

impl StatusOut {
    fn new(socket: &Path, info: DaemonStatusInfo, client: &Client) -> Self {
        Self {
            running: true,
            socket: socket.to_owned(),
            info,
            build: client.welcome().daemon_build.clone(),
            mismatch: MismatchOut::of(client),
        }
    }

    fn human(&self) -> String {
        let i = &self.info;
        let mut out = format!(
            "hutand {} (protocol {}.{}) pid {}, up {}, {} session(s), {} client(s)\nsocket {}\nepoch  {}",
            i.daemon_version,
            i.protocol.major,
            i.protocol.minor,
            i.pid,
            format_uptime(i.uptime_ms / 1000),
            i.sessions,
            i.clients,
            self.socket.display(),
            i.epoch,
        );
        if let Some(build) = &self.build {
            out.push_str(&format!(
                "\nbuild  {}{}\nbinary {}",
                build.version,
                build
                    .git_sha
                    .as_deref()
                    .map(|sha| format!(" ({sha})"))
                    .unwrap_or_default(),
                build.exe_path.display()
            ));
        }
        if let Some(mismatch) = &self.mismatch {
            out.push_str(&format!("\nnote   {}", mismatch.note));
        }
        out
    }
}

#[derive(Serialize)]
struct StartOut {
    started: bool,
    #[serde(flatten)]
    status: StatusOut,
}

#[derive(Serialize)]
struct RestartOut {
    /// A daemon was stopped first (`false`: none was running, one was only started).
    was_running: bool,
    #[serde(flatten)]
    start: StartOut,
}

fn format_uptime(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{}s", secs / 60, secs % 60),
        _ => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
    }
}

fn error_value(body: &ErrorBody) -> Value {
    serde_json::to_value(body).unwrap_or(Value::Null)
}

fn json_text(value: &Value, compact: bool) -> Result<String, CliError> {
    if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    }
    .map_err(io_error)
}

fn io_error(e: serde_json::Error) -> CliError {
    CliError::Failed(format!("encoding output: {e}"))
}

fn print_line(text: &str) -> Result<(), CliError> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}")?;
    Ok(())
}

/// The Hutan session this process runs in (`HUTAN_SESSION_ID`): sent as the client's origin so
/// the daemon applies its `[control]` policy to what runs here (protocol 0.21).
pub(crate) fn session_origin() -> Option<hutan_protocol::messages::Origin> {
    agent::session_from_env().map(|session_id| hutan_protocol::messages::Origin { session_id })
}

/// `hutand` next to this binary (also through symlinks), else the first one on `PATH`.
fn find_hutand() -> Result<PathBuf, CliError> {
    locate_hutand(
        std::env::current_exe().ok().as_deref(),
        std::env::var_os("PATH"),
    )
    .ok_or_else(|| {
        CliError::Failed("hutand not found next to the hutan binary or on PATH".to_owned())
    })
}

/// The executable `hutand` beside `exe`, as invoked and with symlinks resolved (the CLI of an
/// app bundle linked into `/usr/local/bin` finds the bundle's daemon that way), else the first
/// one on `path`.
fn locate_hutand(exe: Option<&Path>, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let beside = exe
        .into_iter()
        .flat_map(|exe| [Some(exe.to_owned()), exe.canonicalize().ok()])
        .flatten()
        .filter_map(|exe| exe.parent().map(|dir| dir.join("hutand")));
    let on_path = path
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("hutand"));
    beside
        .chain(on_path)
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `Hutan.app/Contents/MacOS/{hutan-desktop,hutand,hutan}`; returns the `MacOS` folder.
    fn fake_bundle(root: &Path) -> PathBuf {
        let macos = root.join("Hutan.app/Contents/MacOS");
        for name in ["hutan-desktop", "hutand", "hutan"] {
            executable(&macos.join(name));
        }
        macos
    }

    #[test]
    fn the_cli_of_an_app_bundle_starts_the_bundled_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let macos = fake_bundle(dir.path());
        assert_eq!(
            locate_hutand(Some(&macos.join("hutan")), None),
            Some(macos.join("hutand"))
        );
    }

    #[test]
    fn a_symlink_to_the_bundled_cli_finds_the_bundled_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let macos = fake_bundle(dir.path());
        let bin = dir.path().join("usr/local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(macos.join("hutan"), bin.join("hutan")).unwrap();
        // The link's own folder holds no daemon; the target's does.
        let found = locate_hutand(Some(&bin.join("hutan")), None).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            macos.join("hutand").canonicalize().unwrap()
        );
    }

    #[test]
    fn without_a_daemon_beside_it_the_cli_falls_back_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let lonely = dir.path().join("lonely");
        executable(&lonely.join("hutan"));
        assert_eq!(locate_hutand(Some(&lonely.join("hutan")), None), None);
        let installed = dir.path().join("installed");
        executable(&installed.join("hutand"));
        let path = std::env::join_paths([dir.path().join("empty"), installed.clone()]).unwrap();
        assert_eq!(
            locate_hutand(Some(&lonely.join("hutan")), Some(path)),
            Some(installed.join("hutand"))
        );
    }
}
