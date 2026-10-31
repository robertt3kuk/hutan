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
//! mode = "ask"                   # Mawas: "ask" (ask before acting, the default) or "advisor" (read and propose only)
//! max_actions_per_turn = 10      # Mawas: changes it may make in one turn before every further one asks
//! memory = true                  # Mawas: give a conversation the memory you approved (false: nothing is injected)
//! memory_max_entries = 200       # Mawas: most active memory entries
//! memory_files_in_project = false # Mawas: also keep a workspace's memory as Markdown in its project (`.hutan/memory/`)
//! web = false                    # Mawas: false (the default) starts her harness with web access off (OMP fetch and web search, Claude WebFetch/WebSearch, Codex web_search); true lets her read web pages (docs/control.md § Mawas)
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
//! sound = "default"              # "default": requests and failures play the system's sound; "none": no sound
//! finished = true                # finished work (a turn, a task) raises an intent too, quietly and at most once per 90 s per session
//! digest_model = "off"           # "off", "daily 08:30" (local time) or "every 6h": one model run in a chat of its own, no web, only Hutan's read tools; OMP only (docs/assistant.md § Catching up)
//! digest_model_unverified_omp = false # run it on an OMP version its launch was not verified on
//! digest_hours = 0               # 0 = off. Else: away from every Hutan window this many hours with something that needs you, one quiet notification (docs/assistant.md § Catching up)
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
//!
//! [finish]                       # the user's Finish in Changes / `hutan finish` (protocol 0.35, ADR 0020)
//! push = true                    # push to the upstream after committing (and merging); false: keep it local
//!
//! [sandbox]                      # Contain: Hutan's Seatbelt boundary (protocol 0.52, docs/agents.md § Contain); read at start
//! network = true                 # default for a contained session whose request does not say; false closes the network and every unix socket
//! extra_write = ["~/.cargo"]     # folders every contained session may write besides its checkout, the repository's git directory and the temp folders
//! read_deny = ["~/.ssh"]         # folders no contained session may read (default: Keychains, ~/.ssh, ~/.aws, ~/.gnupg, ~/.config/gh, ~/.netrc; Hutan's own folder always)
//! bin = "/usr/bin/sandbox-exec"  # the Seatbelt tool
//!
//! [agents]                       # what the daemon records about agents (protocol 0.66, docs/assistant.md § Flow)
//! state_log = true               # the log of states agents report, kept 14 days, for how long a terminal agent waited; false stops it and deletes what it holds
//!
//! [updates]                      # the check for a newer Hutan (protocol 0.60, docs/packaging.md § Updates)
//! check = "auto"                 # "auto": ask GitHub's releases at most every 24 h, and on request; "off": only on request
//! channel = "prerelease"         # "prerelease": every release counts; "stable": releases not marked pre-release
//! skip_version = "0.3.2"         # the version the user chose to skip (set by the desktop's "Skip this version")
//!
//! [worktrees]                    # worktree setup and retention (protocol 0.49, docs/orchestration.md § Setup and cleanup)
//! cleanup = "never"              # "never", "merged" (branch merged into the default branch) or "idle" (untouched for cleanup_idle_days)
//! cleanup_idle_days = 14         # with "idle": days without a commit or a change of git state
//! wait_for_setup = true          # an agent chat started in a worktree that is still setting up waits for it
//!
//! [worktrees.projects.hutan]     # per project (name or id): replaces what `.hutan/worktree.toml` says for that key
//! copy = [".env", ".env.local"]  # gitignored files copied from the main checkout (globs; at most 200 files, 5 MiB each)
//! setup = ["pnpm install"]       # run in the new worktree, in a visible terminal session named "Setup"
//! teardown = ["docker compose down"]   # run before the worktree is removed
//!
//! [checkpoints]                  # a snapshot of an agent chat's checkout per turn (ADR 0019, docs/agents.md § Checkpoints)
//! mode = "on"                    # "off": no snapshot from the next turn on
//! max_file_bytes = 10485760      # a larger file is left out (and listed)
//! snapshot_budget_bytes = 67108864   # new content per snapshot; past it the largest files are left out
//! max_files = 5000               # changed files per snapshot
//! max_untracked_files = 20000    # more untracked files and the checkout has no checkpoints
//! exclude = [".env*", "*.pem", "*.key", "*.p12", "id_rsa*", "id_ed25519*", "*.kdbx", "credentials*"]
//! keep_turns = 100               # retention: newest turns per session
//! keep_days = 30                 # retention: a session's checkpoints go after this many days without one
//! max_repo_bytes = 1073741824    # retention: recorded content per repository
//! max_repo_refs = 1000           # retention: checkpoint refs per repository
//!
//! [checkpoints.projects.hutan]   # per project (name or id): only what differs
//! mode = "off"
//!
//! [lsp]                          # language servers (protocol 0.39, docs/lsp.md); read at start
//! enabled = true
//! trust = "ask"                  # "ask": a folder needs the user's decision first; "always"; "never"
//! idle_stop_secs = 300           # a server with no open document stops after this long; 0 = never
//! max_servers = 4                # servers running at once; the least recently used idle one makes room
//!
//! [lsp.servers.rust-analyzer]    # a built-in server (only what differs) or one of your own
//! command = "/Users/me/.cargo/bin/rust-analyzer"   # default: `rust-analyzer` on the login PATH
//! args = []
//! env = { RA_LOG = "warn" }
//! languages = ["rust"]
//! root_markers = ["Cargo.toml", "rust-project.json"]   # the topmost folder with one is the root
//! initialization_options = { cargo = { allTargets = false } }
//! settings = {}                  # what `workspace/configuration` answers
//! idle_stop_secs = 600           # this server only
//! install = "rustup component add rust-analyzer"       # shown when the binary is missing
//! max_failures = 5               # crashes within `failure_window_secs` make it `failed`
//! failure_window_secs = 180
//!
//! [lsp.projects.hutan]           # per project (name or id): only what differs
//! enabled = false                # no server starts for this project's files
//! [lsp.projects.hutan.roots]
//! rust-analyzer = "crates/core"  # this server's root for the project's files (relative to the project's folder)
//!
//! [[hooks]]                      # user hooks (protocol 0.48, docs/integrations.md § User hooks); read at start, only from here
//! on = "turn.finished"           # turn.finished | turn.failed | approval.requested | attention.changed | session.exited | task.done | finish.done
//! harness = "claude"             # optional filters: harness, project, workspace (name or id)
//! run = ["afplay", "/System/Library/Sounds/Glass.aiff"]   # argv, no shell unless `shell = true` (then one command line)
//! timeout = 30                   # seconds, 1-300
//! when_focused = false           # skip while a desktop window is focused
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use hutan_protocol::assistant::AttachmentKind;
use hutan_protocol::notify::IntentKind;
use hutan_protocol::worktree::WorktreeCleanup;
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
    pub finish: FinishSettings,
    pub sandbox: SandboxSettings,
    pub checkpoints: CheckpointSettings,
    pub lsp: LspSettings,
    /// `[[hooks]]`: user hooks (protocol 0.48, `docs/integrations.md` § User hooks); read at
    /// start.
    pub hooks: Vec<HookSettings>,
    pub worktrees: WorktreeSettings,
    /// `[updates]`: the check for a newer Hutan (protocol 0.60, ADR 0024).
    pub updates: UpdatesSettings,
    /// `[agents]`: what the daemon records about agents (protocol 0.66, ADR 0025).
    pub agents: AgentsSettings,
}

/// `[agents]` of `daemon.toml`: the log of agents' states (`docs/assistant.md` § Flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentsSettings {
    /// `true` (default): every change of the state an agent reports about itself (a hook's
    /// report, a terminal agent's status) is logged for 14 days, with the state word, the source,
    /// what a blocked agent waits for and a short redacted message — never output. It is the
    /// only way to tell how long a *terminal* agent waited for the user. `false` stops it and
    /// deletes what the log holds.
    pub state_log: bool,
}

impl Default for AgentsSettings {
    fn default() -> Self {
        Self { state_log: true }
    }
}

/// `[updates]` of `daemon.toml`: when the daemon asks GitHub whether a newer Hutan exists, which
/// releases count, and the version the user skipped (ADR 0024).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesSettings {
    pub check: hutan_protocol::update::UpdateCheck,
    pub channel: hutan_protocol::update::UpdateChannel,
    /// The version the user chose to skip; empty (the default) skips none.
    pub skip_version: String,
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
    /// The managed account new Codex chats run as unless the registry has a default
    /// (`hutan account default`); `system` is the harness's own login (protocol 0.50).
    pub account: Option<String>,
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
    /// The managed account new chats of this harness run as unless the registry has a default
    /// (`hutan account default`); `system` is the harness's own login (protocol 0.50).
    pub account: Option<String>,
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
/// `assistant.max_actions_per_turn` when the setting is absent.
pub const DEFAULT_ASSISTANT_MAX_ACTIONS: u32 = 10;
/// `assistant.memory_max_entries` when the setting is absent.
pub const DEFAULT_ASSISTANT_MEMORY_MAX_ENTRIES: u32 = 200;
/// The shortest `assistant.idle_stop_secs` that is not `0` (never).
pub const MIN_ASSISTANT_IDLE_STOP_SECS: u64 = 60;

/// What Mawas may do (`assistant.mode`, ADR 0017 decision 4, `docs/assistant.md` § Modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMode {
    /// **Ask before acting** (the default): Mawas reads freely, and every change (Hutan's tools
    /// and its harness's own) waits for the user's approval, grants permitting.
    #[default]
    Ask,
    /// **Advisor**: read and propose only; the harness runs with its read-only tools and every
    /// mutating Hutan request is refused.
    Advisor,
}

impl AssistantMode {
    /// The name in `daemon.toml` and in the settings catalog.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Advisor => "advisor",
        }
    }
}

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
            AttachmentKind::AgentTranscript | AttachmentKind::TurnReview => self.agents,
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
        AttachmentKind::AgentTranscript | AttachmentKind::TurnReview => "agents",
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
    /// What Mawas may do: ask before acting, or advise only (`docs/assistant.md` § Modes).
    pub mode: AssistantMode,
    /// Mutating requests Mawas may make in one turn before every further one asks.
    pub max_actions_per_turn: u32,
    /// Whether a Mawas conversation is given the memory the user approved (MW-2): the snapshot
    /// at its start and the deltas at later turns. `false` injects nothing.
    pub memory: bool,
    /// Most active memory entries; a write past it is refused until one is archived.
    pub memory_max_entries: u32,
    /// Whether a workspace's memory is also written as Markdown files in its project
    /// (`<project>/.hutan/memory/`, protocol 0.46). Off by default: they are files of the
    /// user's repository. The global memory's files (in the Home folder) are always written.
    pub memory_files_in_project: bool,
    /// Whether Mawas's harness may reach the web (`MAWAS-NET-GUARD`): OMP's URL reads and web
    /// search, Claude Code's `WebFetch` and `WebSearch`, Codex's `web_search`. Off by default:
    /// Mawas reads other agents' text, which can ask her to fetch a page with a secret in its
    /// address. Read at each start of her harness; ordinary chats are not touched.
    pub web: bool,
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
            mode: AssistantMode::default(),
            max_actions_per_turn: DEFAULT_ASSISTANT_MAX_ACTIONS,
            memory: true,
            memory_max_entries: DEFAULT_ASSISTANT_MEMORY_MAX_ENTRIES,
            memory_files_in_project: false,
            web: false,
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
    /// Whether a notification the daemon posts to the system plays the system's sound
    /// (protocol 0.34). Only a request or a failure ever does; finished work is a banner.
    pub sound: NotifySound,
    /// `false` raises no intent for finished work (`agent_finished`, `chat_turn_finished`,
    /// `task_done`: *Ready for review*); requests and failures are unaffected. A session's
    /// finished turns raise at most one intent per 90 s.
    pub finished: bool,
    /// The digest (protocol 0.68, `docs/assistant.md` § Catching up): when no Hutan window has
    /// been the active one for this many hours and something needs the user (a request that
    /// waits, a failed turn or task, work sent to review) since the last digest, **one** quiet
    /// notification says so. At most one per this many hours, never one per agent; no model is
    /// asked. `0` (the default) is off.
    pub digest_hours: u64,
    /// The scheduled model digest (protocol 0.69, MAWAS-DIGEST-B, ADR 0017 amendment
    /// 2026-10-31): `off` (the default), `daily 08:30` (local time) or `every 6h`. A run asks
    /// a model once, in a chat of its own, with a launch that has no web and no tool but
    /// Hutan's read tools (`docs/assistant.md` § Catching up).
    pub digest_model: DigestSchedule,
    /// Lets the model digest run on an OMP version other than the one its launch was verified
    /// on. Off: another version skips the run and says to re-verify.
    pub digest_model_unverified_omp: bool,
}

/// `notify.digest_model`: when the scheduled model digest runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DigestSchedule {
    #[default]
    Off,
    /// Every day at this time, local time (minutes since midnight).
    Daily { minute_of_day: u16 },
    /// Every this many hours (1 to 168).
    Every { hours: u16 },
}

impl DigestSchedule {
    /// The setting's text, as `FromStr` reads it.
    pub fn text(self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Daily { minute_of_day } => {
                format!("daily {:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
            }
            Self::Every { hours } => format!("every {hours} h"),
        }
    }
}

impl std::str::FromStr for DigestSchedule {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim().to_ascii_lowercase();
        let wrong = || {
            format!(
                "notify.digest_model must be `off`, `daily HH:MM` (local time) or `every N h` \
                 with N from 1 to 168, not `{text}`"
            )
        };
        if text.is_empty() || text == "off" {
            return Ok(Self::Off);
        }
        if let Some(time) = text.strip_prefix("daily") {
            let (h, m) = time.trim().split_once(':').ok_or_else(wrong)?;
            let (h, m): (u16, u16) = (
                h.trim().parse().map_err(|_| wrong())?,
                m.trim().parse().map_err(|_| wrong())?,
            );
            if h > 23 || m > 59 {
                return Err(wrong());
            }
            return Ok(Self::Daily {
                minute_of_day: h * 60 + m,
            });
        }
        if let Some(every) = text.strip_prefix("every") {
            let hours = every.trim().trim_end_matches('h').trim();
            let hours: u16 = hours.parse().map_err(|_| wrong())?;
            if !(1..=168).contains(&hours) {
                return Err(wrong());
            }
            return Ok(Self::Every { hours });
        }
        Err(wrong())
    }
}

impl<'de> Deserialize<'de> for DigestSchedule {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// `notify.sound`: what a posted notification sounds like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifySound {
    /// The system's notification sound.
    #[default]
    Default,
    /// Silent: the banner only.
    None,
}

impl NotifySound {
    /// The `daemon.toml` name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
        }
    }
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            min_turn_secs: DEFAULT_MIN_TURN_SECS,
            // The digest is not an event's intent: it has its own setting (`digest_hours`).
            kinds: IntentKind::ALL
                .into_iter()
                .filter(|kind| *kind != IntentKind::Digest)
                .collect(),
            expire_secs: DEFAULT_EXPIRE_SECS,
            native: true,
            sound: NotifySound::Default,
            finished: true,
            digest_hours: 0,
            digest_model: DigestSchedule::Off,
            digest_model_unverified_omp: false,
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

/// `[finish]` of `daemon.toml`: what the user's Finish does after committing (protocol 0.35,
/// ADR 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FinishSettings {
    /// `true` (default): push the branch to its upstream; `false`: commit (and merge) only. A
    /// request's own `push` wins.
    pub push: bool,
}

impl Default for FinishSettings {
    fn default() -> Self {
        Self { push: true }
    }
}

/// `[sandbox]` of `daemon.toml`: Contain, Hutan's Seatbelt boundary (protocol 0.52, ADR 0021).
/// Read when the daemon starts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxSettings {
    /// Whether a contained session whose request does not say may use the network. `false`
    /// also closes every unix socket (DNS leaks through them).
    pub network: bool,
    /// Folders (`~` allowed) every contained session may write, besides its checkout, the
    /// repository's git directory and the temp folders.
    pub extra_write: Vec<String>,
    /// Folders (`~` allowed) no contained session may read. Hutan's own data folder is always
    /// among them.
    pub read_deny: Vec<String>,
    /// The Seatbelt tool; absent: `/usr/bin/sandbox-exec`.
    pub bin: Option<String>,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            network: true,
            extra_write: Vec::new(),
            read_deny: crate::contain::DEFAULT_READ_DENY
                .iter()
                .map(|path| (*path).to_owned())
                .collect(),
            bin: None,
        }
    }
}

/// `worktrees.cleanup_idle_days` when the setting is absent.
pub const DEFAULT_WORKTREE_IDLE_DAYS: u64 = 14;

/// `[worktrees]` of `daemon.toml`: what happens around a worktree's life (protocol 0.49,
/// `docs/orchestration.md` § Setup and cleanup). What to copy and run is the repository's
/// `.hutan/worktree.toml`; `projects` is the user's own say for one project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeSettings {
    /// Which worktrees `worktree.cleanup` lists. Never removed without the user's say-so.
    pub cleanup: WorktreeCleanup,
    /// With `cleanup = "idle"`: days without a commit or a change of the worktree's git state.
    pub cleanup_idle_days: u64,
    /// An agent chat started in a worktree whose setup runs waits for the setup to end.
    pub wait_for_setup: bool,
    /// `[worktrees.projects.<name or id>]`.
    pub projects: BTreeMap<String, WorktreeProject>,
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            cleanup: WorktreeCleanup::Never,
            cleanup_idle_days: DEFAULT_WORKTREE_IDLE_DAYS,
            wait_for_setup: true,
            projects: BTreeMap::new(),
        }
    }
}

/// `[worktrees.projects.<name or id>]`: a key set here replaces the same key of the repository's
/// `.hutan/worktree.toml`. These commands are the user's own: they run without the trust
/// question the repository's commands need.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeProject {
    pub copy: Option<Vec<String>>,
    pub setup: Option<Vec<String>>,
    pub teardown: Option<Vec<String>>,
}

impl WorktreeSettings {
    /// The override of the project with `id` and `name` (either may be empty): the table of the
    /// id, then the table of the name applied over it.
    pub fn for_project(&self, id: &str, name: &str) -> WorktreeProject {
        let mut project = WorktreeProject::default();
        for key in [id, name] {
            if key.is_empty() {
                continue;
            }
            if let Some(over) = self.projects.get(key) {
                project.copy = over.copy.clone().or(project.copy);
                project.setup = over.setup.clone().or(project.setup);
                project.teardown = over.teardown.clone().or(project.teardown);
            }
        }
        project
    }

    fn check(&self) -> Result<(), String> {
        if self.cleanup_idle_days == 0 {
            return Err("worktrees.cleanup_idle_days must be at least 1".to_owned());
        }
        for (project, over) in &self.projects {
            for (key, list) in [
                ("copy", &over.copy),
                ("setup", &over.setup),
                ("teardown", &over.teardown),
            ] {
                if let Some(list) = list {
                    crate::worktrees::setup_file::check_list(key, list)
                        .map_err(|e| format!("[worktrees.projects.{project}]: {e}"))?;
                }
            }
        }
        Ok(())
    }
}

/// `[checkpoints] mode`: whether agent chat turns get checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointMode {
    #[default]
    On,
    Off,
}

impl CheckpointMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

const MIB: u64 = 1024 * 1024;

/// `[checkpoints]` of `daemon.toml`: checkpoints of agent chat turns (ADR 0019,
/// `docs/agents.md` § Checkpoints). Every value applies from the next snapshot on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointSettings {
    pub mode: CheckpointMode,
    /// A file larger than this is left out of a snapshot.
    pub max_file_bytes: u64,
    /// New content one snapshot may hash; past it the largest files are left out.
    pub snapshot_budget_bytes: u64,
    /// Changed files one snapshot may hash; past it the largest files are left out.
    pub max_files: u64,
    /// A checkout with more untracked, not ignored files has no checkpoints.
    pub max_untracked_files: u64,
    /// File names never captured (`*` matches any run of characters, case ignored).
    pub exclude: Vec<String>,
    /// Retention: the newest turns kept per session.
    pub keep_turns: u64,
    /// Retention: a session's checkpoints go after this many days without a new one.
    pub keep_days: u64,
    /// Retention: new content recorded per repository; past it the oldest turns go.
    pub max_repo_bytes: u64,
    /// Retention: checkpoint refs per repository; past it the oldest turns go.
    pub max_repo_refs: u64,
    /// `[checkpoints.projects.<name or id>]`: what differs for one project.
    pub projects: BTreeMap<String, CheckpointOverride>,
}

impl Default for CheckpointSettings {
    fn default() -> Self {
        Self {
            mode: CheckpointMode::On,
            max_file_bytes: 10 * MIB,
            snapshot_budget_bytes: 64 * MIB,
            max_files: 5_000,
            max_untracked_files: 20_000,
            exclude: crate::checkpoints::caps::DEFAULT_EXCLUDE
                .iter()
                .map(|p| (*p).to_owned())
                .collect(),
            keep_turns: 100,
            keep_days: 30,
            max_repo_bytes: 1024 * MIB,
            max_repo_refs: 1_000,
            projects: BTreeMap::new(),
        }
    }
}

/// `[checkpoints.projects.<name or id>]`: the checkpoint settings of one project that differ
/// from `[checkpoints]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointOverride {
    pub mode: Option<CheckpointMode>,
    pub max_file_bytes: Option<u64>,
    pub snapshot_budget_bytes: Option<u64>,
    pub max_files: Option<u64>,
    pub max_untracked_files: Option<u64>,
}

/// The settings of a project's snapshots: `[checkpoints]` with its override applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectCheckpoints {
    pub mode: CheckpointMode,
    pub max_file_bytes: u64,
    pub snapshot_budget_bytes: u64,
    pub max_files: u64,
    pub max_untracked_files: u64,
}

impl CheckpointSettings {
    /// The settings of the project with `id` and `name` (either may be empty): the overrides of
    /// `[checkpoints.projects.<id>]` and then `[checkpoints.projects.<name>]` applied.
    pub fn for_project(&self, id: &str, name: &str) -> ProjectCheckpoints {
        let mut project = ProjectCheckpoints {
            mode: self.mode,
            max_file_bytes: self.max_file_bytes,
            snapshot_budget_bytes: self.snapshot_budget_bytes,
            max_files: self.max_files,
            max_untracked_files: self.max_untracked_files,
        };
        for key in [id, name] {
            if key.is_empty() {
                continue;
            }
            if let Some(over) = self.projects.get(key) {
                project.mode = over.mode.unwrap_or(project.mode);
                project.max_file_bytes = over.max_file_bytes.unwrap_or(project.max_file_bytes);
                project.snapshot_budget_bytes = over
                    .snapshot_budget_bytes
                    .unwrap_or(project.snapshot_budget_bytes);
                project.max_files = over.max_files.unwrap_or(project.max_files);
                project.max_untracked_files = over
                    .max_untracked_files
                    .unwrap_or(project.max_untracked_files);
            }
        }
        project
    }

    fn check(&self) -> Result<(), String> {
        let numbers = [
            ("max_file_bytes", Some(self.max_file_bytes)),
            ("snapshot_budget_bytes", Some(self.snapshot_budget_bytes)),
            ("max_files", Some(self.max_files)),
            ("max_untracked_files", Some(self.max_untracked_files)),
            ("keep_turns", Some(self.keep_turns)),
            ("keep_days", Some(self.keep_days)),
            ("max_repo_bytes", Some(self.max_repo_bytes)),
            ("max_repo_refs", Some(self.max_repo_refs)),
        ];
        let overrides = self.projects.iter().flat_map(|(project, over)| {
            [
                ("max_file_bytes", over.max_file_bytes),
                ("snapshot_budget_bytes", over.snapshot_budget_bytes),
                ("max_files", over.max_files),
                ("max_untracked_files", over.max_untracked_files),
            ]
            .map(move |(key, value)| (project.as_str(), key, value))
        });
        for (key, value) in numbers {
            if value == Some(0) {
                return Err(format!("checkpoints.{key} must be at least 1"));
            }
        }
        for (project, key, value) in overrides {
            if value == Some(0) {
                return Err(format!(
                    "checkpoints.projects.{project}.{key} must be at least 1"
                ));
            }
        }
        if self.exclude.iter().any(|p| p.trim().is_empty()) {
            return Err("checkpoints.exclude must not contain an empty name".to_owned());
        }
        Ok(())
    }
}

/// `lsp.idle_stop_secs` when the setting is absent.
pub const DEFAULT_LSP_IDLE_STOP_SECS: u64 = 300;
/// `lsp.max_servers` when the setting is absent.
pub const DEFAULT_LSP_MAX_SERVERS: u32 = 4;
/// The most `lsp.max_servers` accepts.
pub const MAX_LSP_MAX_SERVERS: u32 = 32;

/// `[lsp] trust`: what decides a folder the user has not decided about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspTrustMode {
    /// A server does not start until the user trusts the folder.
    #[default]
    Ask,
    /// Every folder is trusted.
    Always,
    /// No folder is trusted: servers never start.
    Never,
}

/// `[lsp]` of `daemon.toml`: language servers (protocol 0.39, `docs/lsp.md`). Read when the
/// daemon starts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspSettings {
    pub enabled: bool,
    pub trust: LspTrustMode,
    /// A server with no open document stops after this many seconds; `0` = never.
    pub idle_stop_secs: u64,
    /// Servers running at once.
    pub max_servers: u32,
    /// `[lsp.servers.<id>]`.
    pub servers: BTreeMap<String, LspServerSettings>,
    /// `[lsp.projects.<name or id>]`.
    pub projects: BTreeMap<String, LspProjectSettings>,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            trust: LspTrustMode::Ask,
            idle_stop_secs: DEFAULT_LSP_IDLE_STOP_SECS,
            max_servers: DEFAULT_LSP_MAX_SERVERS,
            servers: BTreeMap::new(),
            projects: BTreeMap::new(),
        }
    }
}

/// `[lsp.servers.<id>]`: how to run one language server. For a built-in server (`docs/lsp.md`)
/// only what differs is needed; a server of your own needs `command` and `languages`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspServerSettings {
    /// `false`: the server is never used.
    pub enabled: Option<bool>,
    /// A program name looked up on the login `PATH`, or an absolute path.
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    /// Added to the login environment the server starts from.
    pub env: BTreeMap<String, String>,
    /// LSP language ids the server serves.
    pub languages: Option<Vec<String>>,
    /// Files and folders whose topmost occurrence above a document is the server's root.
    pub root_markers: Option<Vec<String>>,
    /// `initializationOptions` of `initialize`.
    pub initialization_options: Option<serde_json::Value>,
    /// What `workspace/configuration` answers.
    pub settings: Option<serde_json::Value>,
    /// Overrides `[lsp] idle_stop_secs` for this server; `0` = never.
    pub idle_stop_secs: Option<u64>,
    /// The command shown (never run) when the binary is missing.
    pub install: Option<String>,
    /// Crashes within `failure_window_secs` after which the server stays `failed`.
    pub max_failures: Option<u32>,
    pub failure_window_secs: Option<u64>,
    /// Documents open in the server at once.
    pub max_documents: Option<usize>,
}

/// `[lsp.projects.<name or id>]`: what differs for the files of one project.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspProjectSettings {
    /// `false`: no server starts for the project's files.
    pub enabled: Option<bool>,
    /// `<server id> = <folder>`: the server's root for the project's files, absolute or
    /// relative to the project's folder, instead of the topmost marker.
    pub roots: BTreeMap<String, String>,
}

impl LspSettings {
    fn check(&self) -> Result<(), String> {
        if self.max_servers == 0 || self.max_servers > MAX_LSP_MAX_SERVERS {
            return Err(format!(
                "lsp.max_servers must be between 1 and {MAX_LSP_MAX_SERVERS}, not {}",
                self.max_servers
            ));
        }
        for (id, server) in &self.servers {
            let bad = |what: &str| Err(format!("[lsp.servers.{id}]: {what}"));
            if id.is_empty() || id.contains(':') || id.contains('/') {
                return Err(format!(
                    "[lsp.servers.{id}]: a server id has no `:` or `/` in it"
                ));
            }
            if server
                .command
                .as_deref()
                .is_some_and(|c| c.trim().is_empty())
            {
                return bad("`command` must not be empty");
            }
            if server
                .languages
                .as_ref()
                .is_some_and(|l| l.iter().any(|l| l.trim().is_empty()))
            {
                return bad("`languages` must not contain an empty name");
            }
            if server
                .root_markers
                .as_ref()
                .is_some_and(|m| m.iter().any(|m| m.trim().is_empty() || m.contains('/')))
            {
                return bad("`root_markers` are names of files or folders, without a `/`");
            }
            if server.max_failures == Some(0) {
                return bad("`max_failures` must be at least 1");
            }
            if server.failure_window_secs == Some(0) {
                return bad("`failure_window_secs` must be at least 1");
            }
            if server.max_documents == Some(0) {
                return bad("`max_documents` must be at least 1");
            }
        }
        for (project, over) in &self.projects {
            for (server, folder) in &over.roots {
                if folder.trim().is_empty() {
                    return Err(format!(
                        "[lsp.projects.{project}.roots]: the root of `{server}` must not be empty"
                    ));
                }
            }
        }
        Ok(())
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

/// Most `[[hooks]]` entries.
pub const MAX_HOOKS: usize = 64;
/// Most arguments of a hook's command, and most bytes of them together.
const MAX_HOOK_ARGS: usize = 64;
const MAX_HOOK_ARG_BYTES: usize = 8 * 1024;

fn default_hook_timeout() -> u32 {
    hutan_protocol::hooks::DEFAULT_TIMEOUT_SECS
}

fn yes() -> bool {
    true
}

/// One `[[hooks]]` entry of `daemon.toml` (user-only; nothing from a repository is ever read
/// here): a command the daemon runs when `on` happens (`docs/integrations.md` § User hooks).
///
/// ```toml
/// [[hooks]]
/// on = "turn.finished"
/// harness = "claude"               # optional filters: harness, project, workspace (name or id)
/// run = ["afplay", "/System/Library/Sounds/Glass.aiff"]   # argv; no shell unless `shell = true`
/// timeout = 30                     # seconds, 1–300 (default 30)
/// when_focused = false             # skip while a desktop window is focused (default: run)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSettings {
    /// A label for the lists; optional.
    #[serde(default)]
    pub name: Option<String>,
    pub on: hutan_protocol::hooks::HookEvent,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// The program and its arguments; with `shell = true` exactly one command line, run by
    /// `/bin/sh -c`.
    pub run: Vec<String>,
    #[serde(default)]
    pub shell: bool,
    /// Seconds before the hook is killed.
    #[serde(default = "default_hook_timeout")]
    pub timeout: u32,
    /// `false`: the hook does not run while a desktop window is focused.
    #[serde(default = "yes")]
    pub when_focused: bool,
}

impl HookSettings {
    fn check(&self, index: usize) -> Result<(), String> {
        let bad = |what: &str| format!("[[hooks]] #{}: {what}", index + 1);
        if self.run.is_empty() || self.run[0].trim().is_empty() {
            return Err(bad("`run` needs a program"));
        }
        if self.shell && self.run.len() != 1 {
            return Err(bad(
                "with `shell = true`, `run` is one command line (a one-element list)",
            ));
        }
        if self.run.len() > MAX_HOOK_ARGS
            || self.run.iter().map(String::len).sum::<usize>() > MAX_HOOK_ARG_BYTES
        {
            return Err(bad(&format!(
                "`run` is longer than {MAX_HOOK_ARGS} arguments or {MAX_HOOK_ARG_BYTES} bytes"
            )));
        }
        if self.run.iter().any(|arg| arg.contains('\0')) {
            return Err(bad("`run` contains a NUL character"));
        }
        if !(1..=hutan_protocol::hooks::MAX_TIMEOUT_SECS).contains(&self.timeout) {
            return Err(bad(&format!(
                "`timeout` must be 1 to {} seconds, not {}",
                hutan_protocol::hooks::MAX_TIMEOUT_SECS,
                self.timeout
            )));
        }
        for (key, value) in [
            ("name", &self.name),
            ("harness", &self.harness),
            ("project", &self.project),
            ("workspace", &self.workspace),
        ] {
            if value.as_ref().is_some_and(|v| v.trim().is_empty()) {
                return Err(bad(&format!("`{key}` is empty")));
            }
        }
        Ok(())
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
    #[serde(default)]
    finish: FinishSettings,
    #[serde(default)]
    sandbox: SandboxSettings,
    #[serde(default)]
    checkpoints: CheckpointSettings,
    #[serde(default)]
    lsp: LspSettings,
    #[serde(default)]
    hooks: Vec<HookSettings>,
    #[serde(default)]
    worktrees: WorktreeSettings,
    #[serde(default)]
    updates: UpdatesSettings,
    #[serde(default)]
    agents: AgentsSettings,
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
        if file.assistant.max_actions_per_turn == 0 {
            return Err(toml::de::Error::custom(
                "assistant.max_actions_per_turn must be at least 1",
            ));
        }
        if file.assistant.memory_max_entries == 0 {
            return Err(toml::de::Error::custom(
                "assistant.memory_max_entries must be at least 1",
            ));
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
        let accounts = file
            .harness
            .iter()
            .map(|(id, h)| (format!("[harness.{id}] account"), &h.account))
            .chain(std::iter::once((
                "[codex] account".to_owned(),
                &file.codex.account,
            )));
        for (key, account) in accounts {
            if let Some(account) = account
                && account != hutan_protocol::account::SYSTEM_ACCOUNT
                && let Err(error) = crate::account_registry::check_name(account)
            {
                return Err(toml::de::Error::custom(format!("{key}: {}", error.message)));
            }
        }
        for (name, server) in &file.mcp {
            check_mcp(name, server)?;
        }
        file.checkpoints.check().map_err(toml::de::Error::custom)?;
        file.lsp.check().map_err(toml::de::Error::custom)?;
        if file.hooks.len() > MAX_HOOKS {
            return Err(toml::de::Error::custom(format!(
                "at most {MAX_HOOKS} [[hooks]] are allowed, not {}",
                file.hooks.len()
            )));
        }
        for (index, hook) in file.hooks.iter().enumerate() {
            hook.check(index).map_err(toml::de::Error::custom)?;
        }
        file.worktrees.check().map_err(toml::de::Error::custom)?;
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
            finish: file.finish,
            sandbox: file.sandbox,
            checkpoints: file.checkpoints,
            lsp: file.lsp,
            hooks: file.hooks,
            worktrees: file.worktrees,
            updates: file.updates,
            agents: file.agents,
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
        assert_eq!(
            notify.kinds,
            IntentKind::ALL
                .into_iter()
                .filter(|kind| *kind != IntentKind::Digest)
                .collect::<Vec<_>>(),
            "every kind an event raises; the digest has its own setting"
        );
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
    fn notify_sound_is_default_or_none() {
        let notify = DaemonSettings::parse("", None).unwrap().notify;
        assert_eq!(notify.sound, NotifySound::Default);
        let silent = DaemonSettings::parse("[notify]\nsound = \"none\"\n", None)
            .unwrap()
            .notify;
        assert_eq!(silent.sound, NotifySound::None);
        assert!(DaemonSettings::parse("[notify]\nsound = \"loud\"\n", None).is_err());
        assert!(DaemonSettings::parse("[notify]\nsound = false\n", None).is_err());
    }

    #[test]
    fn worktrees_never_clean_up_by_default_and_a_project_overrides_what_it_names() {
        let default = DaemonSettings::parse("", None).unwrap().worktrees;
        assert_eq!(default.cleanup, WorktreeCleanup::Never);
        assert_eq!(default.cleanup_idle_days, 14);
        assert!(default.wait_for_setup);
        let text = "[worktrees]\ncleanup = \"idle\"\ncleanup_idle_days = 3\nwait_for_setup = false\n\n\
                    [worktrees.projects.web]\nsetup = [\"pnpm i\"]\n\n\
                    [worktrees.projects.01WS]\ncopy = [\".env\"]\nsetup = [\"npm ci\"]\n";
        let set = DaemonSettings::parse(text, None).unwrap().worktrees;
        assert_eq!(set.cleanup, WorktreeCleanup::Idle);
        assert_eq!(set.cleanup_idle_days, 3);
        assert!(!set.wait_for_setup);
        // The name's table is applied over the id's.
        let project = set.for_project("01WS", "web");
        assert_eq!(project.copy, Some(vec![".env".to_owned()]));
        assert_eq!(project.setup, Some(vec!["pnpm i".to_owned()]));
        assert_eq!(project.teardown, None);
        assert_eq!(set.for_project("other", "x"), WorktreeProject::default());
        for bad in [
            "[worktrees]\ncleanup = \"sometimes\"\n",
            "[worktrees]\ncleanup_idle_days = 0\n",
            "[worktrees]\nmystery = 1\n",
            "[worktrees.projects.p]\nsetup = [\"\"]\n",
            "[worktrees.projects.p]\ncopy = [\"../x\"]\n",
            "[worktrees.projects.p]\nsetup = \"npm ci\"\n",
        ] {
            assert!(DaemonSettings::parse(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn sandbox_defaults_keep_credentials_unreadable_and_the_network_on() {
        let defaults = DaemonSettings::parse("", None).unwrap().sandbox;
        assert!(defaults.network && defaults.extra_write.is_empty() && defaults.bin.is_none());
        assert!(defaults.read_deny.contains(&"~/.ssh".to_owned()));
        let custom = DaemonSettings::parse(
            "[sandbox]\nnetwork = false\nextra_write = [\"~/.cargo\"]\nread_deny = []\n",
            None,
        )
        .unwrap()
        .sandbox;
        assert!(!custom.network && custom.read_deny.is_empty());
        assert_eq!(custom.extra_write, ["~/.cargo"]);
        assert!(DaemonSettings::parse("[sandbox]\nnetwork = \"off\"\n", None).is_err());
        assert!(DaemonSettings::parse("[sandbox]\nprofile = \"x\"\n", None).is_err());
    }

    #[test]
    fn finish_pushes_unless_told_not_to() {
        assert!(DaemonSettings::parse("", None).unwrap().finish.push);
        let local = DaemonSettings::parse("[finish]\npush = false\n", None).unwrap();
        assert!(!local.finish.push);
        assert!(DaemonSettings::parse("[finish]\npush = \"no\"\n", None).is_err());
        assert!(DaemonSettings::parse("[finish]\nforce = true\n", None).is_err());
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

    #[test]
    fn checkpoints_default_on_and_a_project_overrides_what_it_names() {
        let defaults = DaemonSettings::parse("", None).unwrap().checkpoints;
        assert_eq!(defaults.mode, CheckpointMode::On);
        assert_eq!(defaults.max_file_bytes, 10 * MIB);
        assert_eq!(defaults.keep_turns, 100);
        assert_eq!(defaults.max_repo_refs, 1_000);
        let settings = DaemonSettings::parse(
            "[checkpoints]\nmax_files = 7\nkeep_days = 2\n\n\
             [checkpoints.projects.web]\nmode = \"off\"\n\n\
             [checkpoints.projects.01WS]\nmax_file_bytes = 5\n",
            None,
        )
        .unwrap()
        .checkpoints;
        let web = settings.for_project("01OTHER", "web");
        assert_eq!(web.mode, CheckpointMode::Off);
        assert_eq!(web.max_files, 7);
        let by_id = settings.for_project("01WS", "api");
        assert_eq!(by_id.mode, CheckpointMode::On);
        assert_eq!(by_id.max_file_bytes, 5);
        assert_eq!(settings.for_project("", "").max_file_bytes, 10 * MIB);
        for bad in [
            "[checkpoints]\nmode = \"sometimes\"\n",
            "[checkpoints]\nkeep_turns = 0\n",
            "[checkpoints]\nexclude = [\"\"]\n",
            "[checkpoints]\nkeep = 1\n",
            "[checkpoints.projects.web]\nmax_files = 0\n",
            "[checkpoints.projects.web]\nkeep_turns = 3\n",
        ] {
            assert!(DaemonSettings::parse(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn lsp_defaults_ask_for_trust_and_servers_and_projects_are_read_and_checked() {
        let defaults = DaemonSettings::parse("", None).unwrap().lsp;
        assert_eq!(defaults, LspSettings::default());
        assert!(defaults.enabled);
        assert_eq!(defaults.trust, LspTrustMode::Ask);
        assert_eq!(defaults.idle_stop_secs, 300);
        assert_eq!(defaults.max_servers, 4);
        let lsp = DaemonSettings::parse(
            r#"
[lsp]
trust = "always"
idle_stop_secs = 0
max_servers = 2

[lsp.servers.rust-analyzer]
command = "/opt/ra"
env = { RA_LOG = "warn" }
initialization_options = { cargo = { allTargets = false } }
max_failures = 2

[lsp.servers.my-lang]
command = "my-lsp"
args = ["--stdio"]
languages = ["mylang"]
root_markers = ["my.toml"]

[lsp.projects.hutan]
enabled = false
[lsp.projects.hutan.roots]
rust-analyzer = "crates/core"
"#,
            None,
        )
        .unwrap()
        .lsp;
        assert_eq!(lsp.trust, LspTrustMode::Always);
        assert_eq!(lsp.max_servers, 2);
        let ra = &lsp.servers["rust-analyzer"];
        assert_eq!(ra.command.as_deref(), Some("/opt/ra"));
        assert_eq!(ra.env["RA_LOG"], "warn");
        assert_eq!(
            ra.initialization_options,
            Some(serde_json::json!({"cargo": {"allTargets": false}}))
        );
        assert_eq!(ra.max_failures, Some(2));
        assert_eq!(
            lsp.servers["my-lang"].languages,
            Some(vec!["mylang".to_owned()])
        );
        assert_eq!(lsp.projects["hutan"].enabled, Some(false));
        assert_eq!(lsp.projects["hutan"].roots["rust-analyzer"], "crates/core");
        for bad in [
            "[lsp]\ntrust = \"sometimes\"\n",
            "[lsp]\nmax_servers = 0\n",
            "[lsp]\nmax_servers = 33\n",
            "[lsp]\nmystery = 1\n",
            "[lsp.servers.x]\ncommand = \"\"\n",
            "[lsp.servers.x]\nmystery = 1\n",
            "[lsp.servers.\"a:b\"]\ncommand = \"x\"\n",
            "[lsp.servers.x]\nroot_markers = [\"a/b\"]\n",
            "[lsp.servers.x]\nmax_failures = 0\n",
            "[lsp.projects.p.roots]\nx = \"\"\n",
            "[lsp.projects.p]\nmystery = 1\n",
        ] {
            assert!(DaemonSettings::parse(bad, None).is_err(), "{bad}");
        }
    }
    #[test]
    fn hooks_read_every_key_and_default_the_rest() {
        let toml = r#"
[[hooks]]
name = "Phone"
on = "approval.requested"
harness = "claude"
project = "hutan"
workspace = "work"
run = ["curl", "-d", "waiting"]
timeout = 300
when_focused = false

[[hooks]]
on = "finish.done"
shell = true
run = ["echo done >> ~/journal.txt"]
"#;
        let hooks = DaemonSettings::parse(toml, None).unwrap().hooks;
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name.as_deref(), Some("Phone"));
        assert_eq!(
            hooks[0].on,
            hutan_protocol::hooks::HookEvent::ApprovalRequested
        );
        assert_eq!(hooks[0].harness.as_deref(), Some("claude"));
        assert_eq!(hooks[0].timeout, 300);
        assert!(!hooks[0].when_focused);
        assert!(!hooks[0].shell);
        assert_eq!(hooks[1].timeout, 30, "the default timeout");
        assert!(
            hooks[1].when_focused,
            "hooks run while focused unless told not to"
        );
        assert!(hooks[1].shell);
        assert!(DaemonSettings::parse("", None).unwrap().hooks.is_empty());
        // Every event the protocol names is accepted.
        for event in hutan_protocol::hooks::HookEvent::ALL {
            let toml = format!("[[hooks]]\non = \"{}\"\nrun = [\"true\"]\n", event.as_str());
            assert_eq!(
                DaemonSettings::parse(&toml, None).unwrap().hooks[0].on,
                event
            );
        }
    }

    #[test]
    fn a_hook_that_is_not_valid_stops_the_start_and_names_itself() {
        let many = "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\n".repeat(MAX_HOOKS + 1);
        let long_arg = format!(
            "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\", \"{}\"]\n",
            "a".repeat(MAX_HOOK_ARG_BYTES)
        );
        for (bad, mention) in [
            ("[[hooks]]\nrun = [\"true\"]\n", "on"),
            (
                "[[hooks]]\non = \"turn.started\"\nrun = [\"true\"]\n",
                "turn.started",
            ),
            ("[[hooks]]\non = \"turn.finished\"\n", "run"),
            ("[[hooks]]\non = \"turn.finished\"\nrun = []\n", "program"),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\" \"]\n",
                "program",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\ntimeout = 0\n",
                "timeout",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\ntimeout = 301\n",
                "timeout",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"a\", \"b\"]\nshell = true\n",
                "one command line",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\nharness = \"\"\n",
                "harness",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\nmystery = 1\n",
                "mystery",
            ),
            (
                "[[hooks]]\non = \"turn.finished\"\nrun = [\"tr\\u0000ue\"]\n",
                "NUL",
            ),
            (many.as_str(), "at most"),
            (long_arg.as_str(), "arguments or"),
        ] {
            let error = DaemonSettings::parse(bad, None).unwrap_err().to_string();
            assert!(error.contains(mention), "{mention} in {error}");
        }
    }
}
