//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use hutan_core::{ApprovalPolicy, SandboxMode};

/// Exit codes: 0 ok, 1 remote or other error, 2 usage (including an ambiguous session target),
/// 3 hutand unreachable or lost. `attach` killed by SIGTERM/SIGHUP exits 143/129.
#[derive(Debug, Parser)]
#[command(
    name = "hutan",
    version,
    about = "Command-line client for hutand",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// hutand socket; default: $HUTAN_SOCKET, else $HUTAN_HOME/hutand.sock, else the
    /// standard location under ~/Library/Application Support/Hutan
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the hutand daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Workspaces: your own collections of projects, with their own settings and layout
    #[command(subcommand)]
    Ws(WsCommand),
    /// Projects: the repositories and folders sessions work in (made automatically)
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Send one raw protocol request and print the result (or the error body, exit 1)
    Api(ApiArgs),
    /// Print the daemon's current state, then (with --follow) new events, as JSON lines
    Events(EventsArgs),
    /// Start a terminal session
    New(NewArgs),
    /// List sessions
    Ls(LsArgs),
    /// Show the agents and subagents the daemon observes, as a tree per session
    Agents(AgentsArgs),
    /// Show the processes of the live sessions as a tree, with CPU and memory (and hutand)
    Ps(PsArgs),
    /// `ps --watch` ordered by CPU: what is busy right now
    Top(TopArgs),
    /// Kill a session (SIGKILL for its whole process tree), or signal one process of it
    Kill(KillArgs),
    /// Attach this terminal to a session; detach with ctrl+\ then d
    Attach(TargetArgs),
    /// Type text into a session without attaching
    Send(SendArgs),
    /// Stop a session (SIGHUP, then SIGKILL after 3 s)
    Stop(TargetArgs),
    /// Start an ended session again (new terminal, same id)
    Restart(TargetArgs),
    /// What the last hutand restart did to each session (shells respawned, agents resumed, turns
    /// interrupted, sessions not restarted); `hutan recovery ack` dismisses it
    Recovery(RecoveryArgs),
    /// Change a session's title
    Rename(RenameArgs),
    /// Print a session's output (scrollback and screen) as plain text, oldest first
    History(HistoryArgs),
    /// Report a coding agent's state to the session it runs in (for agent hooks; never fails)
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Install, remove, or inspect the agent integrations (Codex, Claude Code, OMP)
    #[command(subcommand)]
    Integration(IntegrationCommand),
    /// Search past sessions: Hutan terminal blocks and Claude Code, Codex and OMP transcripts
    /// (needs `hutan index enable`)
    Search(SearchArgs),
    /// Manage the session history index (opt-in, local)
    #[command(subcommand)]
    Index(IndexCommand),
    /// Chat with a coding agent (Codex, OMP, Claude Code) or directly with a model, through
    /// the daemon
    #[command(subcommand)]
    Chat(ChatCommand),
    /// The logins of the agent harnesses (OMP, Claude Code, Codex): list them, log in or out
    /// through each harness's own flow (Hutan never handles a token)
    #[command(subcommand)]
    Account(AccountCommand),
    /// Model providers for direct chat: OpenAI, Anthropic, Ollama, OpenAI-compatible servers
    #[command(subcommand)]
    Provider(ProviderCommand),
    /// The daemon's own settings (daemon.toml): list, read, change or reset them
    /// (`hutan config ls|get|set|reset`)
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Ask the assistant a question, with context attached (shell output, files, agent
    /// transcripts, search results); streams the answer
    Ask(AskArgs),
    /// Assistant sessions: direct chats with a scope (global, or one workspace)
    #[command(subcommand)]
    Assistant(AssistantCommand),
    /// List the permission requests of agent chats that wait for an answer
    Approvals,
    /// Answer a permission request with one of the options it offers (see `hutan approvals`)
    Approve(ApproveArgs),
    /// List the questions agents ask that wait for an answer
    Questions,
    /// Answer a question of an agent (`hutan answer <id> field=value ...`), or decline or cancel
    /// it (see `hutan questions`)
    Answer(AnswerArgs),
    /// Notification intents the daemon raises when something needs you: list, acknowledge, act
    /// on, or watch them
    #[command(subcommand)]
    Notify(NotifyCommand),
    /// Git worktrees: isolated checkouts for parallel agents (`hutan wt new|ls|rm|diff|merge|commit`)
    #[command(subcommand)]
    Wt(WtCommand),
    /// Tasks with dependencies, run in the main checkout or a worktree (`hutan task add|ls|start|done|review|cancel|reset|rm|show`)
    #[command(subcommand)]
    Task(TaskCommand),
    /// Control the desktop window through the daemon: actions, layout, settings
    /// (`hutan ui actions|layout|run|set`)
    #[command(subcommand)]
    Ui(UiCommand),
    /// Serve Hutan's control tools to an agent as an MCP server on stdio (started by the agent;
    /// see `hutan help agent`)
    Mcp,
    /// Help for a command; `hutan help agent` prints the compact guide for coding agents
    Help(HelpArgs),
}

#[derive(Debug, Args)]
pub struct HelpArgs {
    /// `agent` for the agent guide, or the command to explain
    pub topic: Vec<String>,
}

/// What the desktop window offers, and how to drive it. Needs a running desktop; anything an
/// agent changes this way may wait for the user's approval (`hutan help agent`).
#[derive(Debug, Subcommand)]
pub enum UiCommand {
    /// List the actions and settings the window offers
    Actions(UiActionsArgs),
    /// Show the window's layout: preset, docks, panes with their tabs, focus
    Layout(UiWindowArgs),
    /// Run an action (`hutan ui actions` lists them)
    Run(UiRunArgs),
    /// Change a setting: `hutan ui set terminal.font_size 14`
    Set(UiSetArgs),
}

#[derive(Debug, Args)]
pub struct UiWindowArgs {
    /// The window (default: the one used most recently; `hutan ui layout` shows its id)
    #[arg(long, value_name = "ID")]
    pub window: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiActionsArgs {
    #[command(flatten)]
    pub window: UiWindowArgs,
    /// Only the actions and settings whose id, title or key contains this text
    #[arg(long, value_name = "TEXT")]
    pub filter: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiRunArgs {
    /// The action id, as `hutan ui actions` shows it (`workbench::SplitRight`)
    pub action: String,
    /// An argument of the action: `--arg name=focus`; repeatable. The value is JSON when it
    /// parses as such for the argument's declared type, else text.
    #[arg(long = "arg", value_name = "KEY=VALUE")]
    pub args: Vec<String>,
    #[command(flatten)]
    pub window: UiWindowArgs,
}

#[derive(Debug, Args)]
pub struct UiSetArgs {
    /// `namespace.key`, as `hutan ui actions` shows them (`terminal.cursor.style`)
    pub key: String,
    /// The value: true/false, a number, text, or a JSON list
    pub value: String,
    /// The settings layer to write
    #[arg(long, value_enum, default_value_t = UiLayer::User)]
    pub layer: UiLayer,
    #[command(flatten)]
    pub window: UiWindowArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UiLayer {
    User,
    Workspace,
    Project,
}

impl UiLayer {
    pub fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Workspace => "workspace",
            Self::Project => "project",
        }
    }
}

/// Tasks with dependencies: plan work, run it in the main checkout or a worktree, in a
/// terminal or a Codex chat, and let the daemon start what is ready.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Create a task; prints its id
    Add(TaskAddArgs),
    /// List tasks: dependencies first, each under the task it waits for, with state glyphs
    Ls(TaskLsArgs),
    /// Start a ready task now (it runs in a new session)
    Start(TaskTargetArgs),
    /// Say a task is done (the user's word overrides what the daemon saw)
    Done(TaskTargetArgs),
    /// Accept the finished work of a task, or ask for changes: `hutan task review TASK accept`,
    /// `hutan task review TASK changes -m "what to change"` (the note goes to the task's chat
    /// when its session still lives, and the task runs again)
    Review(TaskReviewArgs),
    /// Give a task up: stops its session when it runs
    Cancel(TaskTargetArgs),
    /// Run an ended task again: back to todo, outcome cleared, earlier sessions kept in its history
    Reset(TaskTargetArgs),
    /// Delete a task; refused while others depend on it unless --cascade
    Rm(TaskRmArgs),
    /// Show everything about a task: fields, dependencies, dependants, sessions
    Show(TaskTargetArgs),
}

const TASK_HELP: &str = "Task: id, unique id prefix (at least 4 characters), or exact title";

#[derive(Debug, Args)]
pub struct TaskAddArgs {
    /// What the task is about (one line)
    pub title: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// A task that must be done first; repeatable
    #[arg(long = "after", value_name = "TASK", help = TASK_HELP)]
    pub after: Vec<String>,
    /// Run in a git worktree: of this branch, or (alone) of `task/<slug of the title>`; made
    /// when there is none. Without it the task runs in the main checkout.
    #[arg(
        long,
        value_name = "BRANCH",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        help = "Run in a git worktree: `--worktree` for task/<slug of the title>, \
                `--worktree=BRANCH` for that branch"
    )]
    pub worktree: Option<String>,
    /// Where a new worktree branch starts (default: HEAD of the repository)
    #[arg(long, value_name = "REF", requires = "worktree")]
    pub base: Option<String>,
    /// The task's prompt: the first message of a Codex chat
    #[arg(long, value_name = "TEXT")]
    pub prompt: Option<String>,
    /// Run it as a Codex chat (needs --prompt); same as `--agent codex`
    #[arg(long, requires = "prompt", conflicts_with_all = ["argv", "agent"])]
    pub codex: bool,
    /// Run it as a chat with this agent: codex, omp or claude (needs --prompt)
    #[arg(long, requires = "prompt", conflicts_with = "argv", value_parser = ["codex", "omp", "claude"])]
    pub agent: Option<String>,
    /// The model of the chat, one the agent offers (`hutan chat models <agent>`); default: the
    /// agent's own
    #[arg(
        long,
        value_name = "MODEL",
        requires = "prompt",
        conflicts_with = "argv"
    )]
    pub model: Option<String>,
    /// Start by itself when ready, within the daemon's `[tasks] max_parallel`
    #[arg(long)]
    pub auto: bool,
    /// What each task given with --after must reach before this one is ready: `done` (its run
    /// finished; the default), `accepted` (a person accepted the work) or `merged` (its branch
    /// was merged)
    #[arg(long = "after-policy", value_name = "WHEN", value_enum, default_value_t = AfterPolicyArg::Done)]
    pub after_policy: AfterPolicyArg,
    /// Command to run in a terminal session after `--`; without a command or --codex starting
    /// the task opens the login shell
    #[arg(last = true, value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[derive(Debug, Args)]
pub struct TaskLsArgs {
    /// Only this workspace's or project's tasks (default: $HUTAN_WORKSPACE, else the project
    /// of the current directory, else all)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct TaskTargetArgs {
    #[arg(help = TASK_HELP)]
    pub target: String,
    /// Which workspace's or project's task an exact title means when several have it (default:
    /// $HUTAN_WORKSPACE, else the project of the current directory)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

/// What `hutan task review` decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReviewArg {
    /// The work is good
    Accept,
    /// The work needs changes (say which with -m)
    Changes,
}

/// `--after-policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AfterPolicyArg {
    Done,
    Accepted,
    Merged,
}

#[derive(Debug, Args)]
pub struct TaskReviewArgs {
    #[arg(help = TASK_HELP)]
    pub target: String,
    #[arg(value_enum)]
    pub decision: ReviewArg,
    /// The note: what to change (sent to the task's chat when its session is live), or a
    /// comment on an acceptance
    #[arg(short = 'm', long = "message", value_name = "NOTE")]
    pub note: Option<String>,
    /// Which workspace's or project's task an exact title means when several have it (default:
    /// $HUTAN_WORKSPACE, else the project of the current directory)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct TaskRmArgs {
    #[arg(help = TASK_HELP)]
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Delete the tasks that depend on it too
    #[arg(long)]
    pub cascade: bool,
}

const WT_WS_HELP: &str = "Workspace or project: id, unique id prefix, or exact name (default: \
     $HUTAN_WORKSPACE, else the project of the current directory)";

#[derive(Debug, Subcommand)]
pub enum WtCommand {
    /// Create a worktree for a branch (a new branch from --base, or the existing one); prints
    /// its path
    New(WtNewArgs),
    /// List the worktrees of a workspace's repositories: branch, path, state, live sessions
    Ls(WtLsArgs),
    /// Remove a worktree; refuses the main one, one with live sessions, and one with uncommitted
    /// changes unless --force
    Rm(WtRmArgs),
    /// Show what a worktree changed against its base branch: its commits since they diverged,
    /// plus uncommitted changes (a unified diff, or --stat)
    Diff(WtDiffArgs),
    /// Merge a worktree's branch into the main checkout's branch (a local commit; never
    /// pushes); refuses a dirty main checkout, a target that is not checked out there, and
    /// conflicts (which are aborted and listed)
    Merge(WtMergeArgs),
    /// Commit everything in a worktree (tracked changes and untracked files that are not
    /// ignored), so that a merge takes it: `hutan wt commit BRANCH -m MSG`
    Commit(WtCommitArgs),
}

#[derive(Debug, Args)]
pub struct WtNewArgs {
    /// The branch to check out; created from --base when it does not exist
    pub branch: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Where a new branch starts (default: HEAD of the repository)
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Directory of the worktree, which must not exist (default: `<repo>-wt/<branch>` next to
    /// the repository, `/` in the branch written as `-`)
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// A directory in the repository (default: the current directory when the project is
    /// found by it, else the workspace's first git root)
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct WtLsArgs {
    /// Only this workspace's or project's repositories (default: $HUTAN_WORKSPACE, else the
    /// project of the current directory, else all)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct WtRmArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`)
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Remove even with uncommitted changes
    #[arg(long)]
    pub force: bool,
    /// Also delete the branch, as `git branch -d` does: an unmerged branch stays and is
    /// reported
    #[arg(long)]
    pub delete_branch: bool,
}

#[derive(Debug, Args)]
pub struct WtDiffArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`); the main checkout works too
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Compare against this branch or commit (default: the repository's default branch)
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Only what is committed: leave out staged, unstaged and untracked changes
    #[arg(long)]
    pub committed: bool,
    /// Print the changed files with their line counts instead of the patch
    #[arg(long)]
    pub stat: bool,
}

#[derive(Debug, Args)]
pub struct WtMergeArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`)
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Merge into this branch, which the main checkout must have checked out (default: the
    /// repository's default branch)
    #[arg(long, value_name = "BRANCH")]
    pub into: Option<String>,
    /// One commit with the branch's changes instead of a merge commit
    #[arg(long, conflicts_with = "rebase")]
    pub squash: bool,
    /// Rebase the branch onto the target, then fast-forward the target (linear history)
    #[arg(long)]
    pub rebase: bool,
    /// The commit message (merge and squash)
    #[arg(short = 'm', long, value_name = "MSG", conflicts_with = "rebase")]
    pub message: Option<String>,
}

#[derive(Debug, Args)]
pub struct WtCommitArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`)
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// The commit message
    #[arg(short = 'm', long, value_name = "MSG")]
    pub message: String,
}

#[derive(Debug, Subcommand)]
pub enum NotifyCommand {
    /// List the pending intents, oldest first
    Ls,
    /// Mark an intent as seen
    Ack(NotifyIdArgs),
    /// Choose an action of an intent: an approval's option answers the approval, `open` asks the
    /// desktop to show the session
    Act(NotifyActArgs),
    /// Print the pending intents, then new ones as they are raised (until interrupted)
    Watch,
}

#[derive(Debug, Args)]
pub struct NotifyIdArgs {
    /// Intent id or unique id prefix (at least 4 characters; see `hutan notify ls`)
    pub intent: String,
}

#[derive(Debug, Args)]
pub struct NotifyActArgs {
    /// Intent id or unique id prefix (at least 4 characters)
    pub intent: String,
    /// One of the intent's actions: an approval option (`accept`, `cancel`, ...) or `open`
    pub action: String,
}

#[derive(Debug, Subcommand)]
pub enum ChatCommand {
    /// Start a chat session with the agent in a directory; prints its id
    New(ChatNewArgs),
    /// Send a message to a chat session
    Send(ChatSendArgs),
    /// Print a chat session's history from the daemon's journal
    Log(TargetArgs),
    /// Follow a chat session live: its recent history, then new events as they happen
    Attach(TargetArgs),
    /// Stop the turn in progress
    Interrupt(TargetArgs),
    /// List the models the agent offers
    Models(ChatModelsArgs),
    /// Switch the model of a direct or harness chat for its next turn (the history stays)
    Model(ChatModelArgs),
}

#[derive(Debug, Args)]
pub struct ChatModelsArgs {
    /// Whose models: codex (default), omp or claude
    #[arg(long, default_value = "codex", value_parser = ["codex", "omp", "claude"])]
    pub agent: String,
}

/// The logins of the agent harnesses.
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// List the harnesses' logins: provider, label, plan, status (never tokens or e-mails)
    Ls,
    /// Log in through the harness's own flow, in a terminal session you complete
    Login(AccountLoginArgs),
    /// Log out through the harness's own flow
    Logout(AccountLoginArgs),
}

#[derive(Debug, Args)]
pub struct AccountLoginArgs {
    /// omp, claude or codex
    #[arg(value_parser = ["omp", "claude", "codex"])]
    pub harness: String,
    /// The provider to log in to (omp: `anthropic`, `openai-codex`, ...; omit to let the
    /// harness ask)
    pub provider: Option<String>,
    /// Attach this terminal to the login session now (detach with ctrl+\ then d)
    #[arg(long)]
    pub attach: bool,
}

#[derive(Debug, Args)]
pub struct ChatModelArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The model (see `hutan provider models <provider>`)
    pub model: String,
    /// Switch to another provider as well (default: keep the chat's provider)
    #[arg(long, value_name = "ID")]
    pub provider: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    /// Configure a provider; a key is stored separately with `hutan provider key`
    Add(ProviderAddArgs),
    /// Store an API key in the Keychain (read from stdin without echo, or from an environment
    /// variable)
    Key(ProviderKeyArgs),
    /// List the configured providers
    Ls,
    /// List the models a provider offers
    Models(ProviderIdArgs),
    /// Forget a provider and its stored key
    Rm(ProviderIdArgs),
    /// Look for Ollama (127.0.0.1:11434) and LM Studio (127.0.0.1:1234) on this machine
    Discover(ProviderDiscoverArgs),
}

#[derive(Debug, Args)]
pub struct ProviderAddArgs {
    /// openai, anthropic, ollama or openai-compatible
    #[arg(value_parser = ["openai", "anthropic", "ollama", "openai-compatible"])]
    pub kind: String,
    /// Display name; the provider's id is made from it (default: the kind)
    #[arg(long)]
    pub name: Option<String>,
    /// Where the provider is (default: the kind's own; required for openai-compatible, e.g.
    /// http://127.0.0.1:1234/v1)
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    /// The model chats use when they name none
    #[arg(long = "model", value_name = "MODEL")]
    pub default_model: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProviderKeyArgs {
    /// The provider's id (see `hutan provider ls`)
    pub provider: String,
    /// Read the key from this environment variable instead of stdin
    #[arg(long, value_name = "VAR", conflicts_with = "clear")]
    pub from_env: Option<String>,
    /// Delete the stored key instead
    #[arg(long)]
    pub clear: bool,
}

#[derive(Debug, Args)]
pub struct ProviderIdArgs {
    /// The provider's id (see `hutan provider ls`)
    pub provider: String,
}

#[derive(Debug, Args)]
pub struct ProviderDiscoverArgs {
    /// Configure the servers that were found and are not configured yet
    #[arg(long)]
    pub add: bool,
}

#[derive(Debug, Args)]
pub struct ChatNewArgs {
    /// Workspace or project to create the session in: id, unique id prefix, or exact name
    /// (default: $HUTAN_WORKSPACE, else the project of the directory, else Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Directory the agent works in (default: the current directory)
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
    /// The agent: codex (default), or omp / claude, the harnesses over ACP (`hutan account ls`
    /// shows which are logged in)
    #[arg(long, value_parser = ["codex", "omp", "claude"], conflicts_with = "direct")]
    pub agent: Option<String>,
    /// Model from `hutan chat models --agent <agent>` (default: the agent catalog's default);
    /// with --direct a model of the provider (default: the provider's default model)
    #[arg(long)]
    pub model: Option<String>,
    /// Session mode of an omp or claude chat, from the modes the agent offers (e.g. plan)
    #[arg(long, value_name = "MODE", conflicts_with = "direct")]
    pub mode: Option<String>,
    /// When the agent asks before acting: untrusted, on-request (default), never
    #[arg(long, value_name = "POLICY", value_parser = parse_policy)]
    pub approval_policy: Option<ApprovalPolicy>,
    /// What the agent may touch: read-only, workspace-write (default), danger-full-access
    #[arg(long, value_name = "MODE", value_parser = parse_sandbox)]
    pub sandbox: Option<SandboxMode>,
    /// Session title (default: `codex · <directory>`, or `<provider> · <model>` with --direct)
    #[arg(long)]
    pub title: Option<String>,
    /// Chat directly with a model of a provider (no agent, no tools); needs --provider
    #[arg(long, requires = "provider")]
    pub direct: bool,
    /// The provider of a direct chat (see `hutan provider ls`)
    #[arg(long, value_name = "ID", requires = "direct")]
    pub provider: Option<String>,
    /// System prompt of a direct chat
    #[arg(long, value_name = "TEXT", requires = "direct")]
    pub system: Option<String>,
}

#[derive(Debug, Args)]
pub struct ChatSendArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The message
    pub text: String,
    /// Stay until the turn ends, printing the reply as it streams (and any approval request);
    /// exits 1 when the turn fails or is cancelled
    #[arg(long)]
    pub wait: bool,
    /// Attach to the message, repeatable: file:<path>[:<first>-<last>], image:<path>, and for
    /// an assistant chat block:<terminal>:<block>, recent:<terminal>:<lines>,
    /// agent:<session>[:<turns>], search:<query>
    #[arg(
        long = "attach",
        value_name = "KIND:VALUE",
        value_parser = crate::attach_spec::parse_attachment
    )]
    pub attach: Vec<hutan_protocol::assistant::Attachment>,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// The question
    pub question: String,
    /// Ask the assistant of this workspace: id, unique id prefix, or exact name (default:
    /// $HUTAN_WORKSPACE, else the global assistant of Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Context to attach, repeatable: block:<terminal>:<block>, recent:<terminal>:<lines>,
    /// agent:<session>[:<turns>], file:<path>[:<first>-<last>], image:<path>, search:<query>
    #[arg(
        long = "attach",
        value_name = "KIND:VALUE",
        value_parser = crate::attach_spec::parse_attachment
    )]
    pub attach: Vec<hutan_protocol::assistant::Attachment>,
}

#[derive(Debug, Subcommand)]
pub enum AssistantCommand {
    /// List the assistant sessions, with their scope and which is the default
    Ls(AssistantLsArgs),
    /// Print the default assistant session of a scope, creating it on first use
    Open(AssistantOpenArgs),
    /// Forget the backend chosen for a scope (the Assistant panel, setup): it uses the parent
    /// workspace's, the global choice, `[assistant]` of daemon.toml, or the automatic backend
    /// again
    Reset(AssistantResetArgs),
}

#[derive(Debug, Args)]
pub struct AssistantLsArgs {
    /// Only the assistants of this workspace (id, unique id prefix, or exact name)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct AssistantOpenArgs {
    /// The workspace: id, unique id prefix, or exact name (default: $HUTAN_WORKSPACE, else the
    /// global assistant of Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Create one more assistant instead of returning the default
    #[arg(long)]
    pub new: bool,
}

#[derive(Debug, Args)]
pub struct AssistantResetArgs {
    /// The workspace whose own choice is removed (id, unique id prefix, or exact name; default:
    /// $HUTAN_WORKSPACE, else the global assistant of Home)
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

/// The daemon's own settings: `daemon.toml`, edited by the daemon with its comments kept.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// List every setting: its value, default, whether a change applies at once (live) or after
    /// a daemon restart, and whether the running daemon still uses an older value (pending)
    Ls,
    /// Print one setting
    Get(ConfigKeyArgs),
    /// Change one setting; the value is TOML: `true`, `30`, `"text"`, `["a", "b"]`, or bare text
    /// for a plain string (`hutan config set control.agents read_only`)
    Set(ConfigSetArgs),
    /// Remove a setting from daemon.toml: back to its default
    Reset(ConfigKeyArgs),
}

#[derive(Debug, Args)]
pub struct ConfigKeyArgs {
    /// A dotted key as `hutan config ls` shows it (`notify.enabled`, `harness.omp.bin`, ...)
    pub key: String,
}

#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// A dotted key as `hutan config ls` shows it (`notify.enabled`, `harness.omp.bin`, ...)
    pub key: String,
    /// The new value as TOML: `true`, `30`, `"text"`, `["a", "b"]`; an empty string removes the
    /// setting like `reset`
    #[arg(allow_hyphen_values = true)]
    pub value: String,
}

#[derive(Debug, Args)]
pub struct ApproveArgs {
    /// Approval id or unique id prefix (at least 4 characters)
    pub approval: String,
    /// One of the options the request offers (`accept`, `cancel`, ...)
    pub option: String,
}

#[derive(Debug, Args)]
pub struct AnswerArgs {
    /// Question id or unique id prefix (at least 4 characters)
    pub question: String,
    /// `field=value` for each field to answer (`hutan questions` shows them). Repeat a field
    /// for a multi-select. A secret field takes `field=-` and reads the value from stdin, so
    /// it never shows in the process list or the shell history.
    #[arg(value_name = "FIELD=VALUE", conflicts_with_all = ["decline", "cancel"])]
    pub values: Vec<String>,
    /// Answer nothing and let the agent go on without it
    #[arg(long, conflicts_with = "cancel")]
    pub decline: bool,
    /// Answer nothing and stop the turn where the agent allows it
    #[arg(long)]
    pub cancel: bool,
}

fn parse_policy(text: &str) -> Result<ApprovalPolicy, String> {
    text.parse()
}

fn parse_sandbox(text: &str) -> Result<SandboxMode, String> {
    text.parse()
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start hutand detached and wait for its socket; does nothing if it already runs
    Start,
    /// Show whether hutand runs, and its pid, uptime, and counts
    Status,
    /// Ask hutand to shut down; sessions that were alive come back at the next start
    Stop {
        /// Stop live sessions first; otherwise shutdown is refused while any run. They are
        /// respawned by the next hutand unless --forget is given
        #[arg(long)]
        stop_sessions: bool,
        /// Record the stopped sessions as stopped so the next hutand does not respawn them
        #[arg(long, requires = "stop_sessions")]
        forget: bool,
    },
    /// Stop hutand and start a new one
    Restart {
        /// Stop live sessions first; otherwise the restart is refused while any run. The new
        /// hutand respawns them
        #[arg(long)]
        stop_sessions: bool,
    },
}

const WS_TARGET_HELP: &str =
    "Workspace id, unique id prefix (at least 4 characters), or exact name";
const PROJECT_TARGET_HELP: &str =
    "Project id, unique id prefix (at least 4 characters), or exact name";

/// Workspaces hold projects; a project is in at most one workspace.
#[derive(Debug, Subcommand)]
pub enum WsCommand {
    /// List workspaces as a tree: each with its projects and session counts, then the projects
    /// in no workspace, then Home (empty projects are hidden unless pinned)
    Ls(WsLsArgs),
    /// Create a workspace, optionally with the projects of some folders
    New {
        name: String,
        /// Folders to add as projects (an existing project of a folder is reused)
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// Add a folder or an existing project to a workspace; a project that is in another
    /// workspace moves
    Add {
        #[arg(help = WS_TARGET_HELP)]
        workspace: String,
        /// A folder (a path: its project is made when there is none) or a project (id, unique
        /// id prefix, or name)
        #[arg(value_name = "PATH|PROJECT")]
        what: String,
    },
    /// Take a project out of a workspace; it stays a project
    Remove {
        #[arg(help = WS_TARGET_HELP)]
        workspace: String,
        #[arg(help = PROJECT_TARGET_HELP)]
        project: String,
    },
    /// Rename a workspace
    Rename {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
        name: String,
    },
    /// Delete a workspace; its projects stay. Refused while sessions of its own are live
    Rm {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
    /// Change the manual order: put a workspace (or project) before another one, or last
    Move(WsMoveArgs),
    /// Keep a workspace at the top of listings
    Pin {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
    /// Undo `pin`
    Unpin {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
    /// Print `export HUTAN_WORKSPACE=<id>`: new sessions of `hutan new` go to that workspace
    /// (use as `eval "$(hutan ws use NAME)"`)
    Use {
        #[arg(help = WS_TARGET_HELP)]
        target: String,
    },
}

/// Projects are repositories (the main checkout and all its worktrees are one project) and
/// folders; the daemon makes them from the folders sessions start in.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List projects with their workspace, session counts and folder (empty ones are hidden
    /// unless pinned or in a workspace)
    Ls(ProjectLsArgs),
    /// Make sure a folder has its project (a repository's project when it is inside one) and
    /// print it; Home's folder and the home directory have none
    Open {
        /// The folder; relative paths are resolved against the current directory
        path: PathBuf,
    },
    /// Rename a project
    Rename {
        #[arg(help = PROJECT_TARGET_HELP)]
        target: String,
        name: String,
    },
    /// Keep a project at the top of listings
    Pin {
        #[arg(help = PROJECT_TARGET_HELP)]
        target: String,
    },
    /// Undo `pin`
    Unpin {
        #[arg(help = PROJECT_TARGET_HELP)]
        target: String,
    },
    /// Close a project (it stays in `project recent`); refused while its sessions are live
    Close {
        #[arg(help = PROJECT_TARGET_HELP)]
        target: String,
    },
    /// Open projects and closed ones remembered, newest first
    Recent {
        /// At most this many (the daemon caps it at 50)
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
    },
}

#[derive(Debug, Args)]
pub struct WsLsArgs {
    /// Order of the listing (pinned workspaces always come first)
    #[arg(long, value_enum, default_value_t = WsSort::Manual)]
    pub sort: WsSort,
    /// Include empty projects that are neither pinned nor in a workspace
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct ProjectLsArgs {
    /// Order of the listing (pinned projects always come first)
    #[arg(long, value_enum, default_value_t = WsSort::Manual)]
    pub sort: WsSort,
    /// Include empty projects that are neither pinned nor in a workspace
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WsSort {
    /// The daemon's manual order (`ws move`)
    Manual,
    /// Latest session activity first
    Recent,
    /// Workspaces and projects with sessions waiting for you (approvals, blocked agents) first
    Attention,
    /// Alphabetical
    Name,
}

#[derive(Debug, Args)]
pub struct WsMoveArgs {
    #[arg(
        help = "Workspace or project: id, unique id prefix (at least 4 characters), or exact name"
    )]
    pub target: String,
    /// Place it right before this workspace or project
    #[arg(
        long,
        value_name = "ID|NAME",
        conflicts_with = "last",
        required_unless_present = "last"
    )]
    pub before: Option<String>,
    /// Place it after all others
    #[arg(long)]
    pub last: bool,
}

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Protocol method, for example `daemon.status`
    pub method: String,
    /// Request params as a JSON value (default `{}`)
    pub params: Option<String>,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Keep running and print events as they happen
    #[arg(long, short)]
    pub follow: bool,
}

const TARGET_HELP: &str =
    "Session id, unique id prefix (at least 4 characters), terminal id (`t:7`), or exact title";

#[derive(Debug, Args)]
pub struct TargetArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
}

#[derive(Debug, Args)]
pub struct RecoveryArgs {
    #[command(subcommand)]
    pub command: Option<RecoveryCommand>,
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    /// Dismiss what a restart did to one session, or to every session (the chips of the desktop
    /// and the RECOVERY column of `hutan ls` go away; nothing else changes)
    Ack(RecoveryAckArgs),
}

#[derive(Debug, Args)]
pub struct RecoveryAckArgs {
    /// Dismiss every session's recovery
    #[arg(long, conflicts_with = "target")]
    pub all: bool,
    #[arg(help = TARGET_HELP, required_unless_present = "all")]
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Workspace or project to create the session in: id, unique id prefix, or exact name
    /// (default: $HUTAN_WORKSPACE, else the project of the working directory, else Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Working directory (default: the current directory)
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,
    /// Session title (default: the command name)
    #[arg(long)]
    pub title: Option<String>,
    /// Extra environment variable for the session; repeatable
    #[arg(long = "env", value_name = "K=V", value_parser = parse_env)]
    pub env: Vec<(String, String)>,
    /// Attach this terminal to the new session
    #[arg(long)]
    pub attach: bool,
    /// Work in the worktree of this branch: created when there is none (see `hutan wt new`),
    /// then the session starts in it and goes to the project of the repository
    #[arg(long, value_name = "BRANCH", conflicts_with = "cwd")]
    pub worktree: Option<String>,
    /// Command to run after `--`; the login shell if omitted
    #[arg(last = true, value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Only sessions of this workspace: id, unique id prefix, or exact name
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Text to type, sent as given
    pub text: String,
    /// Press enter (a carriage return) after the text
    #[arg(long)]
    pub enter: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    pub title: String,
}

/// `hutan history <session>` prints a terminal's output; `hutan history sessions` lists the
/// agent sessions of this machine (so a session cannot be called `sessions` here: use its id).
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub command: Option<HistorySubcommand>,
    #[arg(help = TARGET_HELP, required = true)]
    pub target: Option<String>,
    /// Print at most this many lines, counted from the newest
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..))]
    pub lines: u32,
}

#[derive(Debug, Subcommand)]
pub enum HistorySubcommand {
    /// List the agent sessions on this machine, newest first: Hutan's own chats and terminals and
    /// the transcripts of Claude Code, Codex and OMP (needs `hutan index enable`)
    Sessions(HistorySessionsArgs),
}

#[derive(Debug, Args)]
pub struct HistorySessionsArgs {
    /// Which sessions: everywhere (default), those that ran in a workspace's folders, or those
    /// of one project. `--ws` alone means `workspace`
    #[arg(long, value_enum)]
    pub scope: Option<ScopeArg>,
    /// The workspace or project of `--scope workspace|project`: id, unique id prefix, or name
    /// (default: $HUTAN_WORKSPACE)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Words that must all occur in a session's title, first prompt, latest messages, agent,
    /// model, branch, folder or id; `repo:NAME` and `path:TEXT` narrow to a project or a path
    /// (quote values with spaces)
    #[arg(long, value_name = "TEXT")]
    pub filter: Option<String>,
    /// Order, newest first
    #[arg(long, value_enum, default_value_t = SessionSortArg::Updated)]
    pub sort: SessionSortArg,
    /// Only this source; repeatable
    #[arg(long, value_enum)]
    pub source: Vec<SourceArg>,
    /// Most sessions listed
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    /// Look at the transcript files again first (waits up to 10 s)
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    All,
    Workspace,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionSortArg {
    /// By the last message
    Updated,
    /// By the first message
    Created,
}

/// The `agent` subcommands run inside Hutan sessions on behalf of agent hooks: outside a
/// session (no `HUTAN_SESSION_ID`), without a daemon, on any daemon error, and after 200 ms
/// they do nothing and exit 0, so a hook can never break the agent that runs it.
#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
pub enum AgentCommand {
    /// Report the agent's state (and optionally how to resume it)
    Report(AgentReportArgs),
    /// Say that the agent is gone: forgets its report and resume command
    Release(AgentReleaseArgs),
    /// Stop what an agent is doing without ending the session: the main agent's running turn, or
    /// one subagent (protocol 0.18)
    Interrupt(AgentInterruptArgs),
    /// Translate one Codex or Claude Code hook event (JSON on stdin) into a report; used by the
    /// installed hook scripts
    Hook(AgentHookArgs),
}

#[derive(Debug, Args)]
pub struct AgentReportArgs {
    /// Who reports (one integration, e.g. `claude-hooks`); sequence numbers are per source
    #[arg(long)]
    pub source: String,
    /// The agent's name (`omp`, `claude`, `codex`)
    #[arg(long)]
    pub agent: String,
    #[arg(long, value_enum)]
    pub state: AgentStateArg,
    /// Must increase per session and source; default: the current time in nanoseconds
    #[arg(long)]
    pub seq: Option<u64>,
    /// Short text shown with the state (a permission prompt, a count of subagents)
    #[arg(long)]
    pub message: Option<String>,
    /// Report about this node of the agent tree instead of the main agent (protocol 0.10)
    #[arg(long, value_name = "ID")]
    pub node: Option<String>,
    /// The spawning node (default: the main agent); read when the node is first reported
    #[arg(long, value_name = "ID")]
    pub parent: Option<String>,
    /// `main` (default) or `sub`; only meaningful with --node
    #[arg(long, value_enum, default_value = "main")]
    pub kind: NodeKindArg,
    /// The node's name
    #[arg(long)]
    pub name: Option<String>,
    /// The node's agent definition or role
    #[arg(long)]
    pub agent_type: Option<String>,
    /// The model the node runs
    #[arg(long)]
    pub model: Option<String>,
    /// What the node is doing now (at most 200 characters are kept)
    #[arg(long)]
    pub activity: Option<String>,
    /// Token and cost counters as JSON: `{"kind":"omp.tokens","input":1200,"output":80}`; fields
    /// you do not know stay out (they are never zero-filled)
    #[arg(long, value_name = "JSON")]
    pub usage_json: Option<String>,
    /// Finishes the node
    #[arg(long, value_enum)]
    pub outcome: Option<OutcomeArg>,
    /// The agent's own conversation id
    #[arg(long, value_name = "ID")]
    pub agent_session_id: Option<String>,
    /// The process id of the agent this report is about, when it runs as a process of this
    /// session: `hutan ps` shows it and `hutan agent interrupt` can signal it (protocol 0.18)
    #[arg(long, value_name = "PID")]
    pub pid: Option<u32>,
    /// Command that resumes the agent after a daemon restart, after `--` (a plain command name
    /// on PATH, at most 64 words, no apostrophes)
    #[arg(last = true, value_name = "ARGV")]
    pub resume_argv: Vec<String>,
}

#[derive(Debug, Args)]
pub struct AgentReleaseArgs {
    #[arg(long)]
    pub source: String,
    /// Must increase per session, source and node; default: the current time in nanoseconds
    #[arg(long)]
    pub seq: Option<u64>,
    /// End only this node of the session's agent tree (default: the main agent)
    #[arg(long, value_name = "ID")]
    pub node: Option<String>,
    /// How the node ended (default: completed)
    #[arg(long, value_enum)]
    pub outcome: Option<OutcomeArg>,
}

/// `hutan agents`: the agents (and subagents) of the sessions, as the daemon observes them.
#[derive(Debug, Args)]
pub struct AgentsArgs {
    /// Only this session: id, unique id prefix, or title
    #[arg(value_name = "SESSION")]
    pub target: Option<String>,
    /// Keep running and print the trees again whenever one changes
    #[arg(long)]
    pub watch: bool,
}

/// `hutan ps`: the processes of the live sessions.
#[derive(Debug, Args)]
pub struct PsArgs {
    /// Only this session: id, unique id prefix, terminal id (`t:7`), or exact title
    #[arg(value_name = "SESSION")]
    pub target: Option<String>,
    /// Keep running and print again every --interval milliseconds (the daemon samples only while
    /// somebody watches)
    #[arg(long)]
    pub watch: bool,
    /// Milliseconds between samples with --watch (at least 1000)
    #[arg(long, value_name = "MS", default_value_t = 2000)]
    pub interval: u32,
}

/// `hutan top`: `ps --watch`, busiest first.
#[derive(Debug, Args)]
pub struct TopArgs {
    /// Only this session
    #[arg(value_name = "SESSION")]
    pub target: Option<String>,
    /// Milliseconds between samples (at least 1000)
    #[arg(long, value_name = "MS", default_value_t = 2000)]
    pub interval: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SignalArg {
    Term,
    Kill,
    Int,
    Hup,
    Stop,
    Cont,
}

/// `hutan kill`.
#[derive(Debug, Args)]
pub struct KillArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Signal only this process, which must be the session's own or one of its descendants
    /// (see `hutan ps`); without it the whole session is killed
    #[arg(long, value_name = "PID")]
    pub pid: Option<u32>,
    /// With --pid: the signal to send (default term)
    #[arg(long, value_enum, requires = "pid")]
    pub signal: Option<SignalArg>,
}

/// `hutan agent interrupt`.
#[derive(Debug, Args)]
pub struct AgentInterruptArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// A subagent of the session (the node id `hutan agents` shows); without it the main agent
    #[arg(value_name = "NODE")]
    pub node: Option<String>,
    /// For a subagent that is a process: SIGTERM after SIGINT when it is still there 2 s later
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutcomeArg {
    Completed,
    Failed,
    Interrupted,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NodeKindArg {
    Main,
    Sub,
}

#[derive(Debug, Args)]
pub struct AgentHookArgs {
    /// Whose hook events arrive on stdin
    #[arg(value_enum)]
    pub agent: HookAgentArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentStateArg {
    Idle,
    Working,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookAgentArg {
    Codex,
    Claude,
}

#[derive(Debug, Subcommand)]
pub enum IntegrationCommand {
    /// Add Hutan's hooks to the agent's configuration (idempotent; other entries are kept and
    /// the file is backed up first)
    Install(IntegrationArgs),
    /// Remove only what `install` added
    Uninstall(IntegrationArgs),
    /// Show whether the integration is installed
    Status(IntegrationArgs),
}

#[derive(Debug, Args)]
pub struct IntegrationArgs {
    #[arg(value_enum)]
    pub target: IntegrationTarget,
    /// The agent's configuration directory (default: $CODEX_HOME or ~/.codex; $CLAUDE_CONFIG_DIR
    /// or ~/.claude; $PI_CODING_AGENT_DIR or ~/.omp/agent)
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Manage the `hutan mcp` MCP server entry in the agent's own MCP configuration (Codex
    /// `config.toml`, Claude Code `.claude.json`, OMP `mcp.json`) instead of the hooks. Only
    /// Hutan's marked entry is touched; the file is backed up first. Without this flag no MCP
    /// entry is written or removed.
    #[arg(long)]
    pub mcp: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IntegrationTarget {
    Codex,
    Claude,
    Omp,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Words to find (all must match); `"quoted phrases"` match as phrases
    #[arg(required = true, value_name = "QUERY")]
    pub query: Vec<String>,
    /// Only this source; repeatable
    #[arg(long, value_enum)]
    pub source: Vec<SourceArg>,
    /// Only rows newer than this, like `90m`, `12h`, `7d`, `2w`
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms)]
    pub since: Option<u64>,
    /// Most sessions listed
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: u32,
    /// Instead of listing, open hit N (1-based, as numbered by the same search) in a new
    /// terminal session in its working directory, running the agent's resume command; attaches
    /// when run from a terminal
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub resume: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceArg {
    Hutan,
    Claude,
    Codex,
    Omp,
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// Show whether indexing is on, what it has indexed, and the database size
    Status,
    /// Turn indexing on (recorded in daemon.toml) and index the existing transcripts
    Enable,
    /// Turn indexing off; nothing is read any more, the index stays until `clear`
    Disable,
    /// Delete the index; files are indexed again when they change or on `rebuild`
    Clear,
    /// Read every transcript again from scratch, in the background
    Rebuild,
}

/// `90m`, `12h`, `7d`, `2w` (also `s`) as milliseconds.
fn parse_age_ms(text: &str) -> Result<u64, String> {
    let split = text.len().saturating_sub(1);
    let (number, unit) = text.split_at_checked(split).ok_or("expected like 7d")?;
    let unit_ms: u64 = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => {
            return Err(format!(
                "`{text}`: use s, m, h, d or w after the number, like 7d"
            ));
        }
    };
    let number: u64 = number
        .parse()
        .map_err(|_| format!("`{text}`: expected a number before the unit, like 7d"))?;
    number
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("`{text}` is too long ago"))
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err(format!("expected K=V, got `{text}`")),
    }
}
