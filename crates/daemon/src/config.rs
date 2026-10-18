//! `daemon.toml`: settings only the daemon reads.
//!
//! ```toml
//! [daemon]
//! restore_sessions = "respawn"   # or "stopped"
//!
//! [workspaces]
//! home_folder = "~/Notes"        # the Home workspace's folder; default ~/Hutan
//!
//! [history]                      # session history index, off unless enabled (docs/history-index.md)
//! enabled = false
//! range_days = 0                 # 0 = everything, or 30, or 90
//! hutan = true                  # per-source switches
//! claude = true
//! codex = true
//! omp = true
//!
//! [history.roots]                # override where transcripts live (default: the agents' own dirs)
//! claude = "/path/to/claude/projects"
//!
//! [codex]                        # chat sessions (protocol 0.9)
//! bin = "/Users/me/.local/bin/codex"   # default: `codex` on the daemon's PATH
//! args = ["app-server", "-c", 'model_reasoning_effort="low"']   # default: ["app-server"]
//!
//! [harness.omp]                  # chat through an agent harness over ACP (protocol 0.17, docs/agents.md)
//! bin = "/Users/me/.bun/bin/omp"       # default: `omp` on the login environment's PATH
//! args = ["acp"]                       # default: ["acp"]
//! cli = "/Users/me/.bun/bin/omp"       # the harness's own CLI (account list/login), default: `bin` or `omp`
//!
//! [harness.claude]               # Claude Code through its ACP adapter
//! bin = "/opt/homebrew/bin/claude-agent-acp"  # default: `claude-agent-acp`, else `claude-code-acp`, on the login PATH
//!
//! [mcp.docs]                     # MCP servers handed to harness sessions (no secrets here: credentials stay in the harness)
//! command = "npx"
//! args = ["-y", "some-mcp-server"]
//! [mcp.search]
//! url = "https://mcp.example.com/mcp"  # http (default) or `transport = "sse"`
//!
//! [gateway]                      # direct model chat (protocol 0.11, docs/gateway.md)
//! max_context_chars = 200000     # oldest messages are left out of a request beyond this
//!
//! [assistant]                    # assistant chat (protocol 0.12, docs/assistant.md)
//! backend = "omp"                # omp | claude | codex | provider (protocol 0.20)
//! provider = "openrouter"        # provider of the `provider` backend
//! model = "meta-llama/llama-3.3-70b-instruct"
//! context_budget_chars = 60000   # attached context per message, split over the attachments
//! idle_stop_secs = 600           # a harness assistant idle this long stops its agent process and wakes on the next message; 0 = never, else at least 60
//!
//! [assistant.context]            # what assistants of project workspaces may read
//! shell = true
//! agents = true
//! files = true
//! search = true
//!
//! [assistant.global_context]     # the global (Home) assistant; files are off by default
//! files = false
//!
//! [assistant.workspaces.hutan]  # per workspace (name or id): only what differs
//! files = false
//!
//! [terminal]                     # the environment of new terminal sessions (docs/shell-integration.md)
//! env_inherit = "login"          # "login": a fresh login environment; "daemon": hutand's own
//!
//! [notify]                       # notification intents (protocol 0.13, docs/protocol.md)
//! enabled = true
//! min_turn_secs = 20             # a main agent's turn must take this long to count as "finished"
//! kinds = ["approval_required", "agent_blocked", "agent_finished", "session_exited_with_error",
//!          "chat_turn_finished", "task_done", "task_failed"]   # which intents are raised
//! expire_secs = 3600             # a pending intent (not an approval's) lapses after this long
//! native = true                  # also post intents to the system's notification centre (only from Hutan.app)
//!
//! [tasks]                        # tasks with dependencies (protocol 0.16, docs/orchestration.md)
//! max_parallel = 3               # tasks with `auto_start` start by themselves up to this many at once
//!
//! [projects]                     # automatic projects (protocol 0.19, docs/orchestration.md)
//! auto = true                    # sessions go to the project of their folder; false: Home unless the request names a workspace
//!
//! [control]                      # agents driving Hutan (protocol 0.21, docs/control.md)
//! agents = "ask"                 # "ask" (approval per mutation), "allow", or "read_only"
//! mcp = true                     # Hutan-started harness/Codex sessions get `hutan mcp`
//! approval_timeout_secs = 600    # a pending control approval is refused after this long
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use hutan_protocol::assistant::AttachmentKind;
use hutan_protocol::notify::IntentKind;
use hutan_terminal::EnvInherit;
use serde::Deserialize;
use serde::de::Error as _;

use crate::error::DaemonError;

/// What the daemon does with sessions that were alive when it stopped (ADR 0009 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// Spawn them again with the same ids, titles, and workspaces, in their last known cwd.
    #[default]
    Respawn,
    /// List them as stopped; the user restarts what they want.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DaemonSettings {
    pub restore_sessions: RestoreMode,
    /// Absolute folder of the Home workspace; `None` uses the default
    /// ([`Paths::default_home_folder`](crate::Paths)).
    pub home_folder: Option<PathBuf>,
    pub history: HistorySettings,
    pub codex: CodexSettings,
    /// `[harness.<id>]`: how to start the ACP harnesses (`omp`, `claude`).
    pub harness: BTreeMap<String, HarnessSettings>,
    /// `[mcp.<name>]`: MCP servers passed to harness sessions.
    pub mcp: BTreeMap<String, McpSettings>,
    pub gateway: GatewaySettings,
    pub assistant: AssistantSettings,
    pub notify: NotifySettings,
    pub terminal: TerminalSettings,
    pub tasks: TasksSettings,
    pub projects: ProjectsSettings,
    pub control: ControlSettings,
}

/// `[projects]` of `daemon.toml`: automatic projects (protocol 0.19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProjectsSettings {
    /// `true` (default): a session, chat or task whose request names no workspace goes to the
    /// project of its folder, which the daemon makes when there is none. `false`: it goes to
    /// Home, as before protocol 0.19 (clients that name a workspace still win).
    pub auto: bool,
}

impl Default for ProjectsSettings {
    fn default() -> Self {
        Self { auto: true }
    }
}

/// `[terminal]` of `daemon.toml`: how new terminal sessions get their environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// `login` (default): the child starts from the user's login environment, like a terminal
    /// emulator's tab; `daemon`: it inherits `hutand`'s own environment.
    pub env_inherit: EnvInherit,
}

/// `gateway.max_context_chars` when the setting is absent.
pub const DEFAULT_MAX_CONTEXT_CHARS: usize = 200_000;

/// `[gateway]` of `daemon.toml`: direct model chat.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GatewaySettings {
    /// The most characters of conversation one request carries: the oldest messages that do not
    /// fit are left out (and the turn says so). The system prompt does not count.
    pub max_context_chars: usize,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            max_context_chars: DEFAULT_MAX_CONTEXT_CHARS,
        }
    }
}

/// `[codex]` of `daemon.toml`: how chat sessions start `codex app-server`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct CodexSettings {
    /// The Codex executable; `None` looks for `codex` on `PATH`.
    pub bin: Option<PathBuf>,
    /// The arguments of the executable, replacing the default `["app-server"]` when not empty
    /// (`["app-server", "-c", "key=value"]` adds overrides).
    pub args: Vec<String>,
}

impl CodexSettings {
    pub fn to_config(&self) -> hutan_agents::CodexConfig {
        let mut config = hutan_agents::CodexConfig::default();
        if let Some(bin) = &self.bin {
            config.program.clone_from(bin);
        }
        if !self.args.is_empty() {
            config.args.clone_from(&self.args);
        }
        config
    }
}

/// The harnesses a `[harness.<id>]` table may name.
pub const HARNESS_IDS: [&str; 2] = ["omp", "claude"];

/// `[harness.<id>]` of `daemon.toml`: how the daemon starts an ACP harness and where its own
/// CLI is. Nothing here is secret: logins stay in the harness (ADR 0015).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HarnessSettings {
    /// The ACP executable; `None` looks for the harness's default on `PATH`.
    pub bin: Option<PathBuf>,
    /// Its arguments, replacing the default (`["acp"]` for `omp`, none for `claude`) when not
    /// empty.
    pub args: Vec<String>,
    /// Extra environment of the process (not for secrets: the harness keeps its own logins).
    pub env: BTreeMap<String, String>,
    /// The harness's own CLI for `hutan account` (`omp`, `claude`); `None`: `bin` for `omp`,
    /// else the default name on `PATH`.
    pub cli: Option<PathBuf>,
}

/// `[mcp.<name>]` of `daemon.toml`: one MCP server handed to harness sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpSettings {
    /// A stdio server: the program to run.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// A remote server: its URL.
    pub url: Option<String>,
    /// `http` (default) or `sse`, for `url`.
    pub transport: Option<String>,
}

/// Which source directories the history index reads; `None` = the agent's default location.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct HistoryRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub omp: Option<PathBuf>,
}

/// `[history]` of `daemon.toml`. Nothing is read from any transcript directory unless
/// `enabled` is true.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub enabled: bool,
    /// Index only the last N days: 0 (everything), 30, or 90.
    pub range_days: u32,
    pub hutan: bool,
    pub claude: bool,
    pub codex: bool,
    pub omp: bool,
    pub roots: HistoryRoots,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            range_days: 0,
            hutan: true,
            claude: true,
            codex: true,
            omp: true,
            roots: HistoryRoots::default(),
        }
    }
}

/// `assistant.context_budget_chars` when the setting is absent.
pub const DEFAULT_CONTEXT_BUDGET_CHARS: usize = 60_000;
/// `assistant.idle_stop_secs` when the setting is absent.
pub const DEFAULT_ASSISTANT_IDLE_STOP_SECS: u64 = 600;
/// The shortest `assistant.idle_stop_secs` that is not `0` (never).
pub const MIN_ASSISTANT_IDLE_STOP_SECS: u64 = 60;

/// What an assistant may read when a message attaches it (`docs/assistant.md`). Nothing is read
/// that is not attached; these only decide whether an attachment is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ContextPermissions {
    /// `shell_block`, `shell_recent`: terminal text.
    pub shell: bool,
    /// `agent_transcript`: the journal of a Hutan chat or an indexed transcript.
    pub agents: bool,
    /// `file`: files under the workspace roots.
    pub files: bool,
    /// `search`: the history index.
    pub search: bool,
}

impl Default for ContextPermissions {
    fn default() -> Self {
        Self {
            shell: true,
            agents: true,
            files: true,
            search: true,
        }
    }
}

impl ContextPermissions {
    /// The default of the global assistant: files are off (its folder is Home).
    pub fn global_default() -> Self {
        Self {
            files: false,
            ..Self::default()
        }
    }

    /// Whether an attachment of this kind is accepted. An `image` is a file read (`files`) when
    /// it names a path; inline image data and pasted text are the user's own words and never
    /// gated ([`crate::assistant`] decides which images name a path).
    pub fn allows(self, kind: AttachmentKind) -> bool {
        match kind {
            AttachmentKind::ShellBlock | AttachmentKind::ShellRecent => self.shell,
            AttachmentKind::AgentTranscript => self.agents,
            AttachmentKind::File | AttachmentKind::Image => self.files,
            AttachmentKind::PastedText => true,
            AttachmentKind::Search => self.search,
        }
    }
}

/// The setting that gates an attachment kind, as `daemon.toml` names it.
pub fn gating_setting(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::ShellBlock | AttachmentKind::ShellRecent => "shell",
        AttachmentKind::AgentTranscript => "agents",
        AttachmentKind::File | AttachmentKind::Image | AttachmentKind::PastedText => "files",
        AttachmentKind::Search => "search",
    }
}

/// `[assistant.workspaces.<name or id>]`: the permissions of one workspace that differ from
/// `[assistant.context]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ContextOverride {
    pub shell: Option<bool>,
    pub agents: Option<bool>,
    pub files: Option<bool>,
    pub search: Option<bool>,
}

/// `[assistant]` of `daemon.toml`: the assistant sessions' model and what they may read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AssistantSettings {
    /// What runs new assistants: `omp`, `claude`, `codex` or `provider` (protocol 0.20). Absent
    /// means `provider` when `provider` is set, else the assistant is not configured.
    /// `assistant.configure` (stored by the daemon) overrides it per scope.
    pub backend: Option<hutan_protocol::assistant::AssistantBackend>,
    /// The provider (`hutan provider ls`) of the `provider` backend.
    pub provider: Option<String>,
    /// Their model: a model of the backend's harness (`hutan chat models`), or of the
    /// provider; absent takes the backend's default.
    pub model: Option<String>,
    /// Characters of attached context one message carries, split evenly over its attachments.
    pub context_budget_chars: usize,
    /// Seconds a harness assistant (omp, claude, codex) may sit idle, with no turn, approval or
    /// question pending, before the daemon stops its agent process; the next message starts it
    /// again and resumes the conversation. `0` never stops it; otherwise at least
    /// [`MIN_ASSISTANT_IDLE_STOP_SECS`].
    pub idle_stop_secs: u64,
    /// What assistants of project workspaces may read.
    pub context: ContextPermissions,
    /// What the global assistant (Home) may read.
    pub global_context: ContextPermissions,
    /// Differences per workspace, by workspace name or id.
    pub workspaces: std::collections::BTreeMap<String, ContextOverride>,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            backend: None,
            provider: None,
            model: None,
            context_budget_chars: DEFAULT_CONTEXT_BUDGET_CHARS,
            idle_stop_secs: DEFAULT_ASSISTANT_IDLE_STOP_SECS,
            context: ContextPermissions::default(),
            global_context: ContextPermissions::global_default(),
            workspaces: std::collections::BTreeMap::new(),
        }
    }
}

impl AssistantSettings {
    /// The permissions of a project workspace: `[assistant.context]` with the overrides of
    /// `[assistant.workspaces.<id>]` and then `[assistant.workspaces.<name>]` applied.
    pub fn permissions_for_workspace(&self, name: &str, id: &str) -> ContextPermissions {
        let mut permissions = self.context;
        for key in [id, name] {
            if let Some(over) = self.workspaces.get(key) {
                permissions.shell = over.shell.unwrap_or(permissions.shell);
                permissions.agents = over.agents.unwrap_or(permissions.agents);
                permissions.files = over.files.unwrap_or(permissions.files);
                permissions.search = over.search.unwrap_or(permissions.search);
            }
        }
        permissions
    }
}

/// `notify.min_turn_secs` when the setting is absent.
pub const DEFAULT_MIN_TURN_SECS: u64 = 20;
/// `notify.expire_secs` when the setting is absent.
pub const DEFAULT_EXPIRE_SECS: u64 = 3600;

/// `[notify]` of `daemon.toml`: which notification intents the daemon raises (protocol 0.13).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NotifySettings {
    /// `false` raises no intent at all (`notify.list` stays empty).
    pub enabled: bool,
    /// A main agent's working-to-idle turn raises `agent_finished` only when it took at least
    /// this long.
    pub min_turn_secs: u64,
    /// The kinds that are raised; the others are dropped.
    pub kinds: Vec<IntentKind>,
    /// How long an intent other than an approval's stays pending.
    pub expire_secs: u64,
    /// `false` keeps intents off the system's notification centre even when the daemon could
    /// post them (it runs from `Hutan.app`); they stay available through `notify.list`.
    pub native: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            min_turn_secs: DEFAULT_MIN_TURN_SECS,
            kinds: IntentKind::ALL.to_vec(),
            expire_secs: DEFAULT_EXPIRE_SECS,
            native: true,
        }
    }
}
/// `tasks.max_parallel` when the setting is absent.
pub const DEFAULT_MAX_PARALLEL: usize = 3;

/// `[tasks]` of `daemon.toml`: how many tasks start by themselves at once (protocol 0.16).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TasksSettings {
    /// The most tasks that are running (or blocked) while ready tasks with `auto_start` start
    /// by themselves; the others wait, longest first. Starting a task by hand ignores it.
    pub max_parallel: usize,
}

impl Default for TasksSettings {
    fn default() -> Self {
        Self {
            max_parallel: DEFAULT_MAX_PARALLEL,
        }
    }
}

/// What an agent-originated connection (one whose client runs in a chat session or in a terminal
/// session an agent reported itself in, `ClientInfo.origin`) may change (protocol 0.21, ADR 0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPolicy {
    /// A mutating request becomes an approval (kind `control`) the user answers.
    #[default]
    Ask,
    /// Mutating requests run; every one is still written to the audit table.
    Allow,
    /// Mutating requests are refused with `forbidden`; reads work.
    ReadOnly,
}

/// `control.approval_timeout_secs` when the setting is absent.
pub const DEFAULT_CONTROL_APPROVAL_TIMEOUT_SECS: u64 = 600;

/// `[control]` of `daemon.toml`: the policy for agents driving Hutan through the CLI and
/// `hutan mcp` (`docs/control.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ControlSettings {
    pub agents: AgentPolicy,
    /// Harness and Codex sessions Hutan starts get `hutan mcp` as an MCP server.
    pub mcp: bool,
    /// How long a `control` approval waits for the user; then the request is refused.
    pub approval_timeout_secs: u64,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            agents: AgentPolicy::Ask,
            mcp: true,
            approval_timeout_secs: DEFAULT_CONTROL_APPROVAL_TIMEOUT_SECS,
        }
    }
}

/// The `range_days` values the settings accept.
pub const RANGE_DAYS: [u32; 3] = [0, 30, 90];

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    daemon: DaemonTable,
    #[serde(default)]
    workspaces: WorkspacesTable,
    #[serde(default)]
    history: HistorySettings,
    #[serde(default)]
    codex: CodexSettings,
    #[serde(default)]
    harness: BTreeMap<String, HarnessSettings>,
    #[serde(default)]
    mcp: BTreeMap<String, McpSettings>,
    #[serde(default)]
    gateway: GatewaySettings,
    #[serde(default)]
    assistant: AssistantSettings,
    #[serde(default)]
    notify: NotifySettings,
    #[serde(default)]
    terminal: TerminalSettings,
    #[serde(default)]
    tasks: TasksSettings,
    #[serde(default)]
    projects: ProjectsSettings,
    #[serde(default)]
    control: ControlSettings,
}

#[derive(Debug, Default, Deserialize)]
struct WorkspacesTable {
    home_folder: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DaemonTable {
    #[serde(default)]
    restore_sessions: RestoreMode,
}

impl DaemonSettings {
    /// A missing file means defaults; an unreadable or invalid one is an error, so a typo
    /// does not silently change what happens to the user's sessions. `user_home` is what a
    /// leading `~` in `home_folder` stands for.
    pub fn load(path: &Path, user_home: Option<&Path>) -> Result<Self, DaemonError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(DaemonError::io(format!("reading {}", path.display()))(e)),
        };
        Self::parse(&text, user_home).map_err(|source| DaemonError::Config {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    pub(crate) fn parse(text: &str, user_home: Option<&Path>) -> Result<Self, toml::de::Error> {
        let file: File = toml::from_str(text)?;
        if !RANGE_DAYS.contains(&file.history.range_days) {
            return Err(toml::de::Error::custom(format!(
                "history.range_days must be 0, 30, or 90, not {}",
                file.history.range_days
            )));
        }
        if file.gateway.max_context_chars == 0 {
            return Err(toml::de::Error::custom(
                "gateway.max_context_chars must be at least 1",
            ));
        }
        let home_folder = file
            .workspaces
            .home_folder
            .map(|folder| expand_home_folder(&folder, user_home))
            .transpose()?;
        if file.assistant.context_budget_chars == 0 {
            return Err(toml::de::Error::custom(
                "assistant.context_budget_chars must be at least 1",
            ));
        }
        let idle = file.assistant.idle_stop_secs;
        if idle != 0 && idle < MIN_ASSISTANT_IDLE_STOP_SECS {
            return Err(toml::de::Error::custom(format!(
                "assistant.idle_stop_secs must be 0 (never) or at least {MIN_ASSISTANT_IDLE_STOP_SECS}, not {idle}"
            )));
        }
        if file.notify.expire_secs == 0 {
            return Err(toml::de::Error::custom(
                "notify.expire_secs must be at least 1",
            ));
        }
        if file.tasks.max_parallel == 0 {
            return Err(toml::de::Error::custom(
                "tasks.max_parallel must be at least 1",
            ));
        }
        if let Some(unknown) = file
            .harness
            .keys()
            .find(|id| !HARNESS_IDS.contains(&id.as_str()))
        {
            return Err(toml::de::Error::custom(format!(
                "unknown harness `{unknown}` in [harness.{unknown}] (known: {})",
                HARNESS_IDS.join(", ")
            )));
        }
        for (name, server) in &file.mcp {
            check_mcp(name, server)?;
        }
        Ok(Self {
            restore_sessions: file.daemon.restore_sessions,
            home_folder,
            history: file.history,
            codex: file.codex,
            harness: file.harness,
            mcp: file.mcp,
            gateway: file.gateway,
            assistant: file.assistant,
            notify: file.notify,
            terminal: file.terminal,
            tasks: file.tasks,
            projects: file.projects,
            control: file.control,
        })
    }
}

fn check_mcp(name: &str, server: &McpSettings) -> Result<(), toml::de::Error> {
    let bad = |what: &str| toml::de::Error::custom(format!("[mcp.{name}]: {what}"));
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(bad(
            "the name may only contain letters, digits, `-` and `_`",
        ));
    }
    match (&server.command, &server.url) {
        (Some(command), None) if !command.is_empty() => {}
        (None, Some(url)) if url.starts_with("http://") || url.starts_with("https://") => {}
        (Some(_), Some(_)) => {
            return Err(bad(
                "set either `command` (stdio) or `url` (remote), not both",
            ));
        }
        (None, Some(_)) => return Err(bad("`url` must start with http:// or https://")),
        _ => return Err(bad("set `command` (stdio) or `url` (remote)")),
    }
    if let Some(transport) = &server.transport
        && !matches!(transport.as_str(), "http" | "sse")
    {
        return Err(bad("`transport` is `http` or `sse`"));
    }
    Ok(())
}

/// `~` and `~/...` stand for the user's home directory; the result must be absolute.
fn expand_home_folder(folder: &str, user_home: Option<&Path>) -> Result<PathBuf, toml::de::Error> {
    let path = match folder.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = user_home.ok_or_else(|| {
                toml::de::Error::custom(
                    "workspaces.home_folder uses `~` but there is no home directory",
                )
            })?;
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(folder),
    };
    if !path.is_absolute() {
        return Err(toml::de::Error::custom(format!(
            "workspaces.home_folder must be an absolute path or start with `~/`, got `{folder}`"
        )));
    }
    Ok(path)
}

/// Records `[history] enabled` in `daemon.toml`, creating the file or the table when missing and
/// leaving every other key and comment as it is. Written to a temporary file first, so a crash
/// never leaves a half-written config.
pub fn set_history_enabled(path: &Path, enabled: bool) -> Result<(), DaemonError> {
    let context = |what: &str| format!("{what} {}", path.display());
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(DaemonError::io(context("reading"))(e)),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        DaemonError::io(context("parsing"))(io::Error::new(io::ErrorKind::InvalidData, e))
    })?;
    let history = doc
        .entry("history")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(table) = history.as_table_like_mut() else {
        return Err(DaemonError::io(context("updating"))(io::Error::new(
            io::ErrorKind::InvalidData,
            "`history` is not a table",
        )));
    };
    // Updating in place keeps the comments in front of the key.
    match table.get_mut("enabled") {
        Some(item) => *item = toml_edit::value(enabled),
        None => {
            table.insert("enabled", toml_edit::value(enabled));
        }
    }

    write_atomic(path, &doc.to_string()).map_err(DaemonError::io(context("writing")))
}

/// Replaces the file at `path` with `text`: written to a temporary file beside it (mode 0600),
/// synced, then renamed over it, so a crash never leaves a half-written config.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let tmp = path.with_extension("toml.tmp");
    let write = || -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_defaults_to_codex_on_path_with_app_server() {
        let config = DaemonSettings::parse("", None).unwrap().codex.to_config();
        assert_eq!(config.program, PathBuf::from("codex"));
        assert_eq!(config.args, ["app-server"]);
    }

    #[test]
    fn codex_settings_name_the_binary_and_extra_arguments() {
        let toml = "[codex]\nbin = \"/opt/codex\"\nargs = [\"app-server\", \"-c\", \"model_reasoning_effort=\\\"low\\\"\"]\n";
        let config = DaemonSettings::parse(toml, None).unwrap().codex.to_config();
        assert_eq!(config.program, PathBuf::from("/opt/codex"));
        assert_eq!(
            config.args,
            ["app-server", "-c", "model_reasoning_effort=\"low\""]
        );
    }

    #[test]
    fn the_gateway_context_limit_defaults_and_is_validated() {
        assert_eq!(
            DaemonSettings::parse("", None)
                .unwrap()
                .gateway
                .max_context_chars,
            200_000
        );
        let settings =
            DaemonSettings::parse("[gateway]\nmax_context_chars = 5000\n", None).unwrap();
        assert_eq!(settings.gateway.max_context_chars, 5000);
        assert!(DaemonSettings::parse("[gateway]\nmax_context_chars = 0\n", None).is_err());
    }

    #[test]
    fn defaults_to_respawn() {
        assert_eq!(
            DaemonSettings::parse("", None).unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert_eq!(
            DaemonSettings::parse("[daemon]\n", None)
                .unwrap()
                .restore_sessions,
            RestoreMode::Respawn
        );
    }

    #[test]
    fn reads_the_restore_mode() {
        let parse = |mode: &str| {
            DaemonSettings::parse(&format!("[daemon]\nrestore_sessions = \"{mode}\"\n"), None)
        };
        assert_eq!(
            parse("stopped").unwrap().restore_sessions,
            RestoreMode::Stopped
        );
        assert_eq!(
            parse("respawn").unwrap().restore_sessions,
            RestoreMode::Respawn
        );
        assert!(parse("resurrect").is_err());
    }

    #[test]
    fn unrelated_tables_are_ignored() {
        let settings =
            DaemonSettings::parse("[other]\nx = 1\n[daemon]\nfuture = true\n", None).unwrap();
        assert_eq!(settings, DaemonSettings::default());
    }

    #[test]
    fn history_is_off_by_default_and_reads_its_table() {
        assert!(!DaemonSettings::parse("", None).unwrap().history.enabled);
        let settings = DaemonSettings::parse(
            "[history]\nenabled = true\nrange_days = 30\ncodex = false\n[history.roots]\nclaude = \"/tmp/c\"\n",
            None,
        )
        .unwrap()
        .history;
        assert!(settings.enabled && settings.hutan && settings.claude && settings.omp);
        assert!(!settings.codex);
        assert_eq!(settings.range_days, 30);
        assert_eq!(settings.roots.claude.as_deref(), Some(Path::new("/tmp/c")));
        assert_eq!(settings.roots.codex, None);
    }

    #[test]
    fn history_range_must_be_a_known_value() {
        for days in [0, 30, 90] {
            assert!(
                DaemonSettings::parse(&format!("[history]\nrange_days = {days}\n"), None).is_ok()
            );
        }
        let error = DaemonSettings::parse("[history]\nrange_days = 45\n", None).unwrap_err();
        assert!(error.to_string().contains("range_days"), "{error}");
        assert!(DaemonSettings::parse("[history]\nenabled = \"yes\"\n", None).is_err());
    }

    #[test]
    fn enabling_history_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        set_history_enabled(&path, true).unwrap();
        assert!(DaemonSettings::load(&path, None).unwrap().history.enabled);

        std::fs::write(
            &path,
            "# my settings\n[daemon]\nrestore_sessions = \"stopped\"\n\n[history]\n# off for now\nenabled = false\nrange_days = 90\n",
        )
        .unwrap();
        set_history_enabled(&path, true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my settings") && text.contains("# off for now"),
            "{text}"
        );
        let settings = DaemonSettings::load(&path, None).unwrap();
        assert!(settings.history.enabled);
        assert_eq!(settings.history.range_days, 90);
        assert_eq!(settings.restore_sessions, RestoreMode::Stopped);

        set_history_enabled(&path, false).unwrap();
        assert!(!DaemonSettings::load(&path, None).unwrap().history.enabled);
        assert!(!dir.path().join("daemon.toml.tmp").exists());

        std::fs::write(&path, "[history\n").unwrap();
        assert!(set_history_enabled(&path, true).is_err());
    }

    #[test]
    fn a_missing_file_means_defaults_and_a_bad_one_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(
            DaemonSettings::load(&path, None).unwrap(),
            DaemonSettings::default()
        );
        std::fs::write(&path, "[daemon]\nrestore_sessions = 3\n").unwrap();
        let error = DaemonSettings::load(&path, None).unwrap_err();
        assert!(error.to_string().contains("daemon.toml"), "{error}");
    }

    #[test]
    fn the_home_folder_is_absolute_and_expands_a_leading_tilde() {
        let home = Path::new("/Users/u");
        let folder = |text: &str, user_home| {
            DaemonSettings::parse(
                &format!("[workspaces]\nhome_folder = \"{text}\"\n"),
                user_home,
            )
            .map(|s| s.home_folder)
        };
        assert_eq!(
            folder("/srv/notes", None).unwrap(),
            Some("/srv/notes".into())
        );
        assert_eq!(
            folder("~/Notes", Some(home)).unwrap(),
            Some("/Users/u/Notes".into())
        );
        assert_eq!(folder("~", Some(home)).unwrap(), Some("/Users/u".into()));
        assert!(folder("~/Notes", None).is_err());
        assert!(folder("notes", Some(home)).is_err(), "relative");
        assert!(
            folder("~other/x", Some(home)).is_err(),
            "another user's home"
        );
        assert_eq!(DaemonSettings::parse("", None).unwrap().home_folder, None);
    }

    #[test]
    fn assistant_settings_default_to_everything_but_global_files() {
        let settings = DaemonSettings::parse("", None).unwrap().assistant;
        assert_eq!(settings.provider, None);
        assert_eq!(settings.model, None);
        assert_eq!(settings.context_budget_chars, 60_000);
        assert_eq!(settings.context, ContextPermissions::default());
        assert!(settings.context.files && settings.context.shell);
        assert!(!settings.global_context.files);
        assert!(settings.global_context.shell && settings.global_context.agents);
        assert!(settings.global_context.search);
    }

    #[test]
    fn assistant_settings_are_read_and_overridden_per_workspace() {
        let settings = DaemonSettings::parse(
            "[assistant]\nprovider = \"ollama\"\nmodel = \"llama3\"\ncontext_budget_chars = 900\n\
             [assistant.context]\nshell = false\n\
             [assistant.global_context]\nfiles = true\n\
             [assistant.workspaces.secret]\nfiles = false\nsearch = false\n",
            None,
        )
        .unwrap()
        .assistant;
        assert_eq!(settings.provider.as_deref(), Some("ollama"));
        assert_eq!(settings.model.as_deref(), Some("llama3"));
        assert_eq!(settings.context_budget_chars, 900);
        assert!(!settings.context.shell && settings.context.files);
        assert!(settings.global_context.files);
        let secret = settings.permissions_for_workspace("secret", "01WS");
        assert!(!secret.files && !secret.search && !secret.shell && secret.agents);
        // Another workspace has the plain `[assistant.context]`; the id works as a key too.
        assert_eq!(
            settings.permissions_for_workspace("other", "01WS"),
            settings.context
        );
        let by_id = DaemonSettings::parse("[assistant.workspaces.01WS]\nagents = false\n", None)
            .unwrap()
            .assistant;
        assert!(!by_id.permissions_for_workspace("any", "01WS").agents);
    }

    #[test]
    fn assistant_budget_must_be_positive() {
        assert!(DaemonSettings::parse("[assistant]\ncontext_budget_chars = 0\n", None).is_err());
        assert!(DaemonSettings::parse("[assistant.context]\nshell = \"yes\"\n", None).is_err());
    }

    #[test]
    fn assistant_idle_stop_is_ten_minutes_never_or_at_least_a_minute() {
        let idle =
            |toml: &str| DaemonSettings::parse(toml, None).map(|s| s.assistant.idle_stop_secs);
        assert_eq!(idle("").unwrap(), 600);
        assert_eq!(idle("[assistant]\nidle_stop_secs = 0\n").unwrap(), 0);
        assert_eq!(idle("[assistant]\nidle_stop_secs = 60\n").unwrap(), 60);
        assert_eq!(idle("[assistant]\nidle_stop_secs = 3600\n").unwrap(), 3600);
        let error = idle("[assistant]\nidle_stop_secs = 59\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("assistant.idle_stop_secs"), "{error}");
        assert!(idle("[assistant]\nidle_stop_secs = -1\n").is_err());
    }

    #[test]
    fn terminal_sessions_start_from_the_login_environment_unless_told_otherwise() {
        let inherit =
            |toml: &str| DaemonSettings::parse(toml, None).map(|s| s.terminal.env_inherit);
        assert_eq!(inherit("").unwrap(), EnvInherit::Login);
        assert_eq!(inherit("[terminal]\n").unwrap(), EnvInherit::Login);
        assert_eq!(
            inherit("[terminal]\nenv_inherit = \"login\"\n").unwrap(),
            EnvInherit::Login
        );
        assert_eq!(
            inherit("[terminal]\nenv_inherit = \"daemon\"\n").unwrap(),
            EnvInherit::Daemon
        );
        assert!(inherit("[terminal]\nenv_inherit = \"shell\"\n").is_err());
        assert!(inherit("[terminal]\nenv_inherit = true\n").is_err());
    }

    #[test]
    fn notify_defaults_raise_everything_with_a_twenty_second_turn() {
        let notify = DaemonSettings::parse("", None).unwrap().notify;
        assert!(notify.enabled);
        assert_eq!(notify.min_turn_secs, 20);
        assert_eq!(notify.kinds, IntentKind::ALL.to_vec());
        assert_eq!(notify.expire_secs, 3600);
        assert!(notify.native, "native posting is on unless switched off");
    }

    #[test]
    fn notify_settings_are_read_and_validated() {
        let notify = DaemonSettings::parse(
            "[notify]\nenabled = false\nmin_turn_secs = 0\nexpire_secs = 5\n\
             kinds = [\"approval_required\", \"chat_turn_finished\"]\n",
            None,
        )
        .unwrap()
        .notify;
        assert!(!notify.enabled);
        assert!(notify.native);
        assert_eq!(notify.min_turn_secs, 0);
        assert_eq!(notify.expire_secs, 5);
        assert_eq!(
            notify.kinds,
            [IntentKind::ApprovalRequired, IntentKind::ChatTurnFinished]
        );
        let unknown = DaemonSettings::parse("[notify]\nkinds = [\"gossip\"]\n", None).unwrap_err();
        assert!(unknown.to_string().contains("gossip"), "{unknown}");
        assert!(DaemonSettings::parse("[notify]\nexpire_secs = 0\n", None).is_err());
        assert!(DaemonSettings::parse("[notify]\nenabled = \"yes\"\n", None).is_err());
    }

    #[test]
    fn notify_native_can_be_switched_off() {
        let notify = DaemonSettings::parse("[notify]\nnative = false\n", None)
            .unwrap()
            .notify;
        assert!(!notify.native);
        assert!(notify.enabled, "intents are still raised");
        assert!(DaemonSettings::parse("[notify]\nnative = 1\n", None).is_err());
    }

    #[test]
    fn task_settings_default_to_three_in_parallel_and_are_validated() {
        let defaults = DaemonSettings::parse("", None).unwrap().tasks;
        assert_eq!(defaults.max_parallel, 3);
        assert_eq!(defaults, TasksSettings::default());
        let tasks = DaemonSettings::parse("[tasks]\nmax_parallel = 8\n", None)
            .unwrap()
            .tasks;
        assert_eq!(tasks.max_parallel, 8);
        assert!(DaemonSettings::parse("[tasks]\nmax_parallel = 0\n", None).is_err());
        assert!(DaemonSettings::parse("[tasks]\nmax_parallel = -1\n", None).is_err());
        assert!(DaemonSettings::parse("[tasks]\nmax_parallel = \"3\"\n", None).is_err());
    }

    #[test]
    fn automatic_projects_are_on_unless_switched_off() {
        assert!(DaemonSettings::parse("", None).unwrap().projects.auto);
        assert!(DaemonSettings::default().projects.auto);
        let off = DaemonSettings::parse("[projects]\nauto = false\n", None).unwrap();
        assert!(!off.projects.auto);
        assert!(DaemonSettings::parse("[projects]\nauto = \"no\"\n", None).is_err());
        assert!(DaemonSettings::parse("[projects]\nauto = true\nmystery = 1\n", None).is_ok());
    }

    #[test]
    fn the_default_intent_kinds_include_tasks() {
        let notify = DaemonSettings::parse("", None).unwrap().notify;
        assert!(notify.kinds.contains(&IntentKind::TaskDone));
        assert!(notify.kinds.contains(&IntentKind::TaskFailed));
        let only = DaemonSettings::parse("[notify]\nkinds = [\"task_failed\"]\n", None)
            .unwrap()
            .notify;
        assert_eq!(only.kinds, [IntentKind::TaskFailed]);
    }

    #[test]
    fn harnesses_and_mcp_servers_are_read_and_checked() {
        let none = DaemonSettings::parse("", None).unwrap();
        assert!(none.harness.is_empty() && none.mcp.is_empty());
        let settings = DaemonSettings::parse(
            r#"
[harness.omp]
bin = "/opt/omp"
args = ["acp", "--profile", "work"]
env = { OMP_PROFILE = "work" }
cli = "/opt/omp-cli"

[harness.claude]
bin = "/opt/claude-code-acp"

[mcp.docs]
command = "npx"
args = ["-y", "docs-server"]

[mcp.search]
url = "https://mcp.example.com/mcp"
transport = "sse"
"#,
            None,
        )
        .unwrap();
        let omp = &settings.harness["omp"];
        assert_eq!(omp.bin.as_deref(), Some(Path::new("/opt/omp")));
        assert_eq!(omp.args, ["acp", "--profile", "work"]);
        assert_eq!(omp.env["OMP_PROFILE"], "work");
        assert_eq!(omp.cli.as_deref(), Some(Path::new("/opt/omp-cli")));
        assert_eq!(settings.harness["claude"].args, Vec::<String>::new());
        assert_eq!(settings.mcp["docs"].command.as_deref(), Some("npx"));
        assert_eq!(settings.mcp["search"].transport.as_deref(), Some("sse"));

        for bad in [
            // a harness Hutan has no adapter for, a typo in a key
            "[harness.gemini]\nbin = \"x\"\n",
            "[harness.omp]\nbinary = \"x\"\n",
            // an MCP server needs exactly one of command and url
            "[mcp.x]\n",
            "[mcp.x]\ncommand = \"a\"\nurl = \"https://a\"\n",
            "[mcp.x]\nurl = \"ftp://a\"\n",
            "[mcp.x]\nurl = \"https://a\"\ntransport = \"websocket\"\n",
            "[mcp.\"a b\"]\ncommand = \"a\"\n",
        ] {
            assert!(DaemonSettings::parse(bad, None).is_err(), "{bad}");
        }
    }
}
