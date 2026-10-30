//! Command-line grammar. clap reports usage errors itself with exit code 2.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use hutan_core::{ApprovalPolicy, SafetyPreset, SandboxMode};

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

    /// EXPERIMENTAL (off until Settings ▸ Experimental turns it on; use at your own risk): run
    /// the command against the hutand of a host from `[[host]]` in settings.toml, over SSH (see
    /// `hutan host list`); the host's own daemon is started there when it is not running
    #[arg(long, global = true, value_name = "NAME", conflicts_with = "socket")]
    pub host: Option<String>,

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
    /// Close a session: end it and remove it from the lists (refused while something runs in it
    /// unless --force); `hutan reopen` brings it back
    Close(CloseArgs),
    /// Bring a closed session back: a terminal starts its command again in its last directory, a
    /// chat continues its conversation
    Reopen(TargetArgs),
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
    /// Checkpoints of agent turns: the checkout's files before and after each turn of an agent
    /// chat, which the user can rewind to (`hutan checkpoint ls|diff|rm|restore|undo`)
    #[command(subcommand)]
    Checkpoint(CheckpointCommand),
    /// The logins of the agent harnesses (OMP, Claude Code, Codex): list them, log in or out
    /// through each harness's own flow (Hutan never handles a token)
    #[command(subcommand)]
    Account(AccountCommand),
    /// How much each harness reports used and left of its limits (5-hour, weekly), and the
    /// tokens and cost of the turns Hutan ran today and this week
    Usage(UsageArgs),
    /// Updates: whether a newer Hutan exists and update it the way it was installed
    /// (`hutan update status|check|install`)
    #[command(subcommand)]
    Update(UpdateCommand),
    /// Model providers for direct chat: OpenAI, Anthropic, Ollama, OpenAI-compatible servers
    #[command(subcommand)]
    Provider(ProviderCommand),
    /// The daemon's own settings (daemon.toml): list, read, change or reset them
    /// (`hutan config ls|get|set|reset`)
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Ask Mawas, the assistant, a question, with context attached (shell output, files, agent
    /// transcripts, search results); streams the answer
    Ask(AskArgs),
    /// Mawas, the assistant: its chats, with a scope (global, or one workspace)
    #[command(subcommand)]
    Assistant(AssistantCommand),
    /// What Mawas may do without asking: the grants its approvals stored (`hutan grants
    /// ls|revoke`)
    #[command(subcommand)]
    Grants(GrantsCommand),
    /// Your hooks: commands the daemon runs on Hutan events, set in `[[hooks]]` of daemon.toml
    /// (`hutan hooks list|test|log`)
    #[command(subcommand)]
    Hooks(HooksCommand),
    /// Mawas's memory: what the assistant remembers because you approved it (`hutan memory
    /// ls|add|edit|archive|restore|history|proposals`)
    #[command(subcommand)]
    Memory(MemoryCommand),
    /// Files through the daemon, the one place permissions are enforced: list, print, write
    /// (atomically, with a version check), find and search (`hutan fs ls|cat|stat|write|find|search`),
    /// and make, move, copy and trash them (`hutan fs mkdir|touch|mv|cp|trash|restore`)
    #[command(subcommand)]
    Fs(FsCommand),
    /// A session's terminal output: search the scrollback and the screen
    /// (`hutan terminal search SESSION QUERY`)
    #[command(subcommand)]
    Terminal(TerminalCommand),
    /// Read-only git history through the daemon: the commits of a repository, a folder or a file
    /// (renames followed), who last changed each line, one commit with its diff
    /// (`hutan git log|blame|show`)
    #[command(subcommand)]
    Git(GitCommand),
    /// Language servers the daemon runs for the editor and for agents: list them, restart or stop
    /// one, trust a folder (`hutan lsp ls|status|restart|stop|trust`)
    Lsp(LspArgs),
    /// Errors and warnings of a file (or what the servers know about a folder), from its language
    /// server; works without the Hutan window
    Diagnostics(DiagnosticsArgs),
    /// Open `path[:line[:col]]` in a file tab of the Hutan window (looked up through the
    /// daemon); without a window, print where the file is
    Open(OpenArgs),
    /// List the permission requests of agent chats that wait for an answer
    Approvals,
    /// Answer a permission request with one of the options it offers (see `hutan approvals`)
    Approve(ApproveArgs),
    /// List the questions agents ask that wait for an answer
    Questions,
    /// Answer a question of an agent (`hutan answer <id> field=value ...`), or decline or cancel
    /// it (see `hutan questions`)
    Answer(AnswerArgs),
    /// Notification intents the daemon raises when something wants you: list, acknowledge, act
    /// on, or watch them
    #[command(subcommand)]
    Notify(NotifyCommand),
    /// Git worktrees: isolated checkouts for parallel agents (`hutan wt new|ls|rm|diff|merge|commit`)
    #[command(subcommand)]
    Wt(WtCommand),
    /// Finish the work in a checkout: commit everything, merge a worktree into its target, push
    /// the branch to its upstream (never forced). Only you can run it, not an agent
    Finish(FinishArgs),
    /// EXPERIMENTAL, off until Settings ▸ Experimental turns it on: hosts reached over SSH
    /// (list, add, remove, test, forward; `hutan --host NAME <command>`)
    #[command(subcommand)]
    Host(HostCommand),
    /// Bridge standard input and output to this machine's hutand socket, bytes only (what `ssh
    /// HOST hutan proxy` runs for `--host`; it starts the daemon when it is not running)
    #[command(hide = true)]
    Proxy,
    /// Tasks with dependencies, run in the main checkout or a worktree (`hutan task add|ls|start|done|review|cancel|reset|rm|show`)
    #[command(subcommand)]
    Task(TaskCommand),
    /// Color and icon themes: list, validate, create and export packs. Local files only: it
    /// never starts or contacts the daemon (`hutan theme ls|validate|new|export`)
    #[command(subcommand)]
    Theme(ThemeCommand),
    /// Control the desktop window through the daemon: actions, layout, settings
    /// (`hutan ui actions|layout|run|set`)
    #[command(subcommand)]
    Ui(UiCommand),
    /// Open Hutan's setup (Run Setup Again…) in the desktop window: agents, Vim mode, send key,
    /// history, start at login, command line, appearance, each starting at today's value
    Setup,
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
    /// The account a chat task's agent runs as (`hutan account list`; `system`: the harness's
    /// own login); default: the project's default, else the harness's
    #[arg(
        long,
        value_name = "NAME",
        requires = "prompt",
        conflicts_with = "argv"
    )]
    pub account: Option<String>,
    /// Safety preset of a chat task's agent: ask, contained, read_only or full (`hutan chat new
    /// --help` says what each means); refused when the task is made if the agent cannot honour
    /// it. Default: the agent's own settings
    #[arg(
        long,
        value_name = "PRESET",
        value_parser = parse_safety,
        requires = "prompt",
        conflicts_with = "argv"
    )]
    pub safety: Option<SafetyPreset>,
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
    /// its path. Copies the files and runs the setup `.hutan/worktree.toml` asks for, unless
    /// --no-setup
    #[command(visible_alias = "create")]
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
    /// Stage files or hunks: copy them from the working tree into git's index (the working tree
    /// is never touched): `hutan wt stage BRANCH src/a.rs`, `--hunk ID`, `--all`
    Stage(WtIndexArgs),
    /// Unstage files or hunks: take them out of git's index again; the working tree is never
    /// touched
    Unstage(WtIndexArgs),
    /// Discard working-tree changes of files or hunks, after asking. A checkpoint is taken first:
    /// `hutan checkpoint restore ID` (printed) puts everything back
    Discard(WtDiscardArgs),
    /// Run a worktree's setup again (`.hutan/worktree.toml`: copy, setup); commands that come
    /// from the repository ask for your confirmation the first time and whenever they change
    Setup(WtSetupArgs),
    /// Remove the worktrees the `[worktrees] cleanup` policy lists (merged, or idle for N days),
    /// each after its teardown; --dry-run only lists them. Never removes one with a live
    /// session or uncommitted changes
    Prune(WtPruneArgs),
}

#[derive(Debug, Args)]
pub struct WtSetupArgs {
    /// The worktree's path, or its branch (see `hutan wt ls`)
    pub target: String,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// Confirm the repository's commands without asking (needed without a terminal)
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct WtPruneArgs {
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// List what would be removed and remove nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Remove without asking (needed without a terminal)
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Whose branch is merged into the default branch, whatever `[worktrees] cleanup` says
    #[arg(long, conflicts_with = "idle_days")]
    pub merged: bool,
    /// Untouched for this many days, whatever `[worktrees] cleanup` says
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u64).range(1..))]
    pub idle_days: Option<u64>,
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
    /// Do not copy files or run the setup of `.hutan/worktree.toml` (`hutan wt setup` runs it
    /// later)
    #[arg(long)]
    pub no_setup: bool,
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
    /// Print the hunks that can be staged, unstaged or discarded, with their ids, instead of
    /// the patch
    #[arg(long, conflicts_with = "stat")]
    pub hunks: bool,
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
    /// Commit only what is staged (see `hutan wt stage`), nothing else; works in the main
    /// checkout too
    #[arg(long)]
    pub staged: bool,
}

#[derive(Debug, Args)]
pub struct WtIndexArgs {
    /// The checkout: a worktree's path or branch, or a main checkout's path (see `hutan wt ls`)
    pub target: String,
    /// Files (or folders), relative to the checkout's top level
    pub paths: Vec<String>,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// A hunk, by the id `hutan wt diff --hunks` prints (repeatable)
    #[arg(long = "hunk", value_name = "ID")]
    pub hunks: Vec<String>,
    /// With exactly one --hunk: only these changed lines of it (numbers into its body, from 0,
    /// comma separated; `hutan wt diff --hunks --json` has the bodies)
    #[arg(long, value_name = "N,N", value_delimiter = ',', requires = "hunks")]
    pub lines: Vec<u32>,
    /// Every file of the checkout (not with files or hunks)
    #[arg(long, conflicts_with_all = ["paths", "hunks"])]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct WtDiscardArgs {
    /// The checkout: a worktree's path or branch, or a main checkout's path (see `hutan wt ls`)
    pub target: String,
    /// Files (or folders), relative to the checkout's top level: their working tree is made
    /// equal to the index (an untracked file is deleted)
    pub paths: Vec<String>,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// An unstaged hunk, by the id `hutan wt diff --hunks` prints (repeatable)
    #[arg(long = "hunk", value_name = "ID")]
    pub hunks: Vec<String>,
    /// With exactly one --hunk: only these changed lines of it (numbers into its body, from 0)
    #[arg(long, value_name = "N,N", value_delimiter = ',', requires = "hunks")]
    pub lines: Vec<u32>,
    /// Also discard what is staged for the files (back to HEAD)
    #[arg(long)]
    pub staged: bool,
    /// Discard without asking (required when stdin is not a terminal)
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct FinishArgs {
    /// The checkout: a worktree's path or branch, or a main checkout's path (default: the
    /// checkout of the current directory)
    pub target: Option<String>,
    #[arg(long = "ws", value_name = "ID|NAME", help = WT_WS_HELP)]
    pub workspace: Option<String>,
    /// The commit message for the uncommitted changes (and a squash's commit); needed when there
    /// are any
    #[arg(short = 'm', long, value_name = "MSG")]
    pub message: Option<String>,
    /// Commit (and merge) only; do not push. Without it, `[finish] push` of daemon.toml decides
    /// (default: push)
    #[arg(long)]
    pub no_push: bool,
    /// A worktree: merge into this branch, which the main checkout must have checked out
    /// (default: the repository's default branch)
    #[arg(long, value_name = "BRANCH")]
    pub into: Option<String>,
    /// A worktree: one commit with the branch's changes instead of a merge commit
    #[arg(long, conflicts_with = "rebase")]
    pub squash: bool,
    /// A worktree: rebase the branch onto the target, then fast-forward the target
    #[arg(long)]
    pub rebase: bool,
    /// Commit only what is staged (see `hutan wt stage`); the rest stays uncommitted
    #[arg(long)]
    pub staged: bool,
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
    /// Silence a session's notifications: it raises none (no banner, no sound) until unmuted,
    /// and what it has pending is withdrawn. It still shows as needing you in the app.
    Mute(TargetArgs),
    /// Let a muted session's notifications through again
    Unmute(TargetArgs),
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

/// Checkpoints of agent turns (ADR 0019): the checkout's files before and after each turn of an
/// agent chat in a git repository, kept in hidden refs of that repository.
#[derive(Debug, Subcommand)]
pub enum CheckpointCommand {
    /// List checkpoints, newest first: when, files and lines changed, state, files left out
    Ls(CheckpointLsArgs),
    /// Show what a checkpoint's turn changed (a unified diff, or --stat), or what changed since
    /// its start (--against now, --against CHECKPOINT)
    Diff(CheckpointDiffArgs),
    /// Delete a checkpoint, or every checkpoint of a session (their refs and rows): what they
    /// kept can no longer be restored
    Rm(CheckpointRmArgs),
    /// Restore the files a checkpoint's turn changed to how they were before it: prints the
    /// preview, asks (default No), leaves conflicts alone unless named; files only (the agent
    /// still remembers, git history is not rewound). The restore is a checkpoint too: undo it
    /// with `hutan checkpoint undo`
    Restore(CheckpointRestoreArgs),
    /// Undo the latest restore (restore the files it changed to how they were before it);
    /// undoing again undoes the undo
    Undo(CheckpointUndoArgs),
}

#[derive(Debug, Args)]
pub struct CheckpointLsArgs {
    /// Only this session's: session id, unique id prefix (at least 4 characters), or exact
    /// title; a closed session by its full id
    pub session: Option<String>,
    /// Only the sessions of this workspace or project (id, unique id prefix, or exact name)
    #[arg(long = "ws", value_name = "ID|NAME", conflicts_with = "session")]
    pub workspace: Option<String>,
    /// At most this many (1 to 200; default 50)
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
    /// The page after this checkpoint (what the last page said)
    #[arg(long, value_name = "CHECKPOINT")]
    pub before: Option<String>,
}

#[derive(Debug, Args)]
pub struct CheckpointDiffArgs {
    /// Checkpoint id, or a unique id prefix (see `hutan checkpoint ls`)
    pub checkpoint: String,
    /// Only this file or directory, relative to the checkout
    pub path: Option<String>,
    /// Compare the checkpoint's start with `end` (what its turn changed), `now` (the checkout as
    /// it is) or another checkpoint (through the end of that one's turn)
    #[arg(long, value_name = "end|now|CHECKPOINT", default_value = "end")]
    pub against: String,
    /// Print the changed files with their line counts instead of the patch
    #[arg(long)]
    pub stat: bool,
}

#[derive(Debug, Args)]
pub struct CheckpointRmArgs {
    /// A checkpoint (id or unique id prefix), or a session (id, prefix or title): every
    /// checkpoint of it
    pub target: String,
}

#[derive(Debug, Args)]
pub struct CheckpointRestoreArgs {
    /// Restore the files to before this checkpoint's turn (id or unique id prefix)
    pub checkpoint: String,
    /// Rewind through this later checkpoint of the same session too (every turn in between)
    #[arg(long, value_name = "CHECKPOINT")]
    pub through: Option<String>,
    /// Only this file or directory, relative to the checkout (repeatable)
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<String>,
    /// Overwrite this conflict too: a file changed since the turn, between the turns or while
    /// it ran (repeatable; the preview lists them)
    #[arg(long = "accept-conflicts", value_name = "PATH")]
    pub accept_conflicts: Vec<String>,
    /// Restore without asking (required when stdin is not a terminal)
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct CheckpointUndoArgs {
    /// Undo the latest restore of this session (id, unique id prefix, or exact title; a closed
    /// session by its full id); without it, the latest restore of any session
    pub session: Option<String>,
    /// Overwrite this conflict too (repeatable; the preview lists them)
    #[arg(long = "accept-conflicts", value_name = "PATH")]
    pub accept_conflicts: Vec<String>,
    /// Undo without asking (required when stdin is not a terminal)
    #[arg(long, short = 'y')]
    pub yes: bool,
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
    /// Stop the turn in progress; messages queued for the chat are dropped unless --keep-queue
    Interrupt(ChatInterruptArgs),
    /// Send the chat's last message again (after a failed turn); it is not journaled twice
    Retry(TargetArgs),
    /// Start a new chat with this chat's conversation up to a turn; the parent is not changed
    Fork(ChatForkArgs),
    /// List the models the agent offers
    Models(ChatModelsArgs),
    /// Switch the model of a direct or harness chat for its next turn (the history stays)
    Model(ChatModelArgs),
    /// The extra folders of a session: list, add, remove (you only: an agent cannot)
    #[command(subcommand)]
    Dirs(ChatDirsCommand),
    /// What the chat's context holds and what its turns spent: context used and window, tokens
    /// and cost per turn, tools, files, time, and what the agent never reports. `--measure`
    /// first has OMP or Claude Code print its context by category (/context: a turn in the
    /// conversation that costs no tokens)
    Insight(ChatInsightArgs),
}

#[derive(Debug, Args)]
pub struct ChatInsightArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Ask the agent for its context by category first (OMP and Claude Code; it appears in the
    /// conversation as the message /context and its answer)
    #[arg(long)]
    pub measure: bool,
}

#[derive(Debug, Subcommand)]
pub enum ChatDirsCommand {
    /// List the session's folders and how far the agent has taken each (now, next turn, restart,
    /// paste, unsupported)
    #[command(visible_alias = "ls")]
    List(TargetArgs),
    /// Add folders to a chat or terminal session. A running Codex or OMP chat takes them at
    /// once or with its next turn; Claude Code and a contained OMP need a restart; a terminal
    /// agent (OMP, Claude Code) is typed `/add-dir <path>` without Enter, for you to confirm
    Add(ChatDirsEditArgs),
    /// Take extra folders out of a session
    #[command(visible_alias = "rm")]
    Remove(ChatDirsEditArgs),
}

#[derive(Debug, Args)]
pub struct ChatDirsEditArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The folders (absolute, or relative to the current directory)
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ChatModelsArgs {
    /// Whose models: codex (default), omp or claude
    #[arg(long, default_value = "codex", value_parser = ["codex", "omp", "claude"])]
    pub agent: String,
}

#[derive(Debug, Subcommand)]
pub enum UpdateCommand {
    /// The running version, how Hutan was installed, and the newer release the daemon knows of
    /// (contacts nothing)
    Status,
    /// Ask GitHub's releases now, whatever `updates.check` says, and print the status; the one
    /// request carries only a `User-Agent: Hutan/<version>` header
    Check,
    /// Update Hutan the way it was installed: the daemon runs Homebrew in a visible terminal
    /// session (`hutan attach` it to watch), or, for a Hutan.app copied by hand, downloads,
    /// verifies and unpacks the new app (`--apply` then installs it). Running sessions are not
    /// touched; after the update the app relaunches, and an idle daemon moves onto the new build
    /// by itself
    Install(UpdateInstallArgs),
}

#[derive(Debug, Args)]
pub struct UpdateInstallArgs {
    /// Wait for the update to finish and exit with its status (0 only when it succeeded): for
    /// Homebrew until it exits, for a copied app until the new app is downloaded and verified
    /// (with `--apply`, until it is in place)
    #[arg(long)]
    pub wait: bool,
    /// A Hutan.app copied by hand whose new app is downloaded and verified: have the daemon
    /// move the old app to the Trash and put the new one in its place (and, with `--after-pid`,
    /// open it)
    #[arg(long)]
    pub apply: bool,
    /// With `--apply`: wait until this process has exited (the app that is quitting) before the
    /// app is replaced, then open the new one
    #[arg(long, value_name = "PID", requires = "apply")]
    pub after_pid: Option<u32>,
}

#[derive(Debug, Args)]
pub struct UsageArgs {
    /// Answer from what the daemon already knows; do not ask the harnesses (OMP's own report,
    /// Codex's) what is left, which takes a few seconds and goes over the network
    #[arg(long)]
    pub cached: bool,
}

/// The logins of the agent harnesses, and the accounts Hutan keeps for them.
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// List the harnesses' own logins and the accounts you added: provider, label, plan,
    /// status, defaults (never tokens or e-mails)
    #[command(visible_alias = "ls")]
    List,
    /// Log in through the harness's own flow, in a terminal session you complete
    Login(AccountLoginArgs),
    /// Log out through the harness's own flow
    Logout(AccountLoginArgs),
    /// Add an account for a harness: an empty folder Hutan keeps, which the harness fills when
    /// you log in (one account per work/personal subscription)
    Add(AccountAddArgs),
    /// Remove an account: the harness logs out of it, its folder is deleted
    #[command(visible_alias = "rm")]
    Remove(AccountRemoveArgs),
    /// Set the account new sessions of a harness take (or of one project with --project)
    Default(AccountDefaultArgs),
}

#[derive(Debug, Args)]
pub struct AccountLoginArgs {
    /// omp, claude or codex
    #[arg(value_parser = ["omp", "claude", "codex"])]
    pub harness: String,
    /// The provider to log in to (omp: `anthropic`, `openai-codex`, ...; omit to let the
    /// harness ask)
    pub provider: Option<String>,
    /// The account to log in (see `hutan account list`); default: the harness's own login
    #[arg(long, value_name = "NAME")]
    pub account: Option<String>,
    /// Attach this terminal to the login session now (detach with ctrl+\ then d)
    #[arg(long)]
    pub attach: bool,
}

#[derive(Debug, Args)]
pub struct AccountAddArgs {
    /// omp, claude or codex
    #[arg(value_parser = ["omp", "claude", "codex"])]
    pub harness: String,
    /// The account's name: 1 to 32 lowercase letters, digits, `-` or `_`
    pub name: String,
    /// What to call it (shown in the desktop); default: the name
    #[arg(long)]
    pub label: Option<String>,
    /// Only create the account; do not start the harness's login for it
    #[arg(long)]
    pub no_login: bool,
    /// Attach this terminal to the login session now (detach with ctrl+\ then d)
    #[arg(long, conflicts_with = "no_login")]
    pub attach: bool,
}

#[derive(Debug, Args)]
pub struct AccountRemoveArgs {
    /// omp, claude or codex
    #[arg(value_parser = ["omp", "claude", "codex"])]
    pub harness: String,
    pub name: String,
    /// Do not ask
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct AccountDefaultArgs {
    /// omp, claude or codex
    #[arg(value_parser = ["omp", "claude", "codex"])]
    pub harness: String,
    /// An account name, or `system` for the harness's own login; omit with --clear
    #[arg(required_unless_present = "clear", conflicts_with = "clear")]
    pub account: Option<String>,
    /// Remove the default instead
    #[arg(long)]
    pub clear: bool,
    /// Set it for this project or workspace (id, prefix or exact name) instead of the harness
    #[arg(long, value_name = "ID|NAME")]
    pub project: Option<String>,
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
    /// A further folder the agent works in besides --cwd; repeatable, at most 8 (also
    /// `hutan chat dirs add` later)
    #[arg(long = "dir", value_name = "PATH", conflicts_with = "direct")]
    pub dirs: Vec<PathBuf>,
    /// Chat directly with a model of a provider (no agent, no tools); needs --provider
    #[arg(long, requires = "provider")]
    pub direct: bool,
    /// The provider of a direct chat (see `hutan provider ls`)
    #[arg(long, value_name = "ID", requires = "direct")]
    pub provider: Option<String>,
    /// System prompt of a direct chat
    #[arg(long, value_name = "TEXT", requires = "direct")]
    pub system: Option<String>,
    /// The account the agent runs as (`hutan account list`; `system` is the harness's own
    /// login). Default: the project's default, else the harness's
    #[arg(long, value_name = "NAME", conflicts_with = "direct")]
    pub account: Option<String>,
    /// Safety preset: ask, contained (no questions inside a boundary: the agent's own sandbox for
    /// Codex and Claude Code, Hutan's Seatbelt for OMP, which can read your files but writes
    /// only in its worktree), read_only or full (no boundary, no questions). A harness that
    /// cannot honour it refuses with the reason; replaces --approval-policy, --sandbox and
    /// --mode. Default: the agent's own settings
    #[arg(
        long,
        value_name = "PRESET",
        value_parser = parse_safety,
        conflicts_with_all = ["direct", "approval_policy", "sandbox", "mode"]
    )]
    pub safety: Option<SafetyPreset>,
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
    /// a Mawas chat block:<terminal>:<block>, recent:<terminal>:<lines>,
    /// agent:<session>[:<turns>], search:<query>
    #[arg(
        long = "attach",
        value_name = "KIND:VALUE",
        value_parser = crate::attach_spec::parse_attachment
    )]
    pub attach: Vec<hutan_protocol::assistant::Attachment>,
    /// While a turn runs, wait in the chat's queue and go when it ends (at once when no turn
    /// runs); text only. Prints the queued message's id
    #[arg(long, conflicts_with_all = ["now", "wait", "attach"])]
    pub queue: bool,
    /// Do not wait for the turn: the agent takes the message at its next step where it can
    /// (Codex); any other agent's turn is interrupted and the message goes next. Text only
    #[arg(long, conflicts_with_all = ["queue", "wait", "attach"])]
    pub now: bool,
}

#[derive(Debug, Args)]
pub struct ChatInterruptArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Keep the messages queued for the chat; they wait until you send again
    #[arg(long)]
    pub keep_queue: bool,
}

/// `hutan chat fork`.
#[derive(Debug, Args)]
pub struct ChatForkArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The turn the new chat keeps through: its number (1 is the first) or its id; default: the
    /// latest turn
    #[arg(long)]
    pub turn: Option<String>,
    /// How to cut: `exact` after the turn (what the agent can do: direct chats and Codex at any
    /// turn, OMP only at the latest), `head` the whole conversation as it is now, `summary` a
    /// fresh conversation given a digest of the turns up to --turn
    #[arg(long, default_value = "exact", value_parser = ["exact", "head", "summary"])]
    pub mode: String,
    /// Name the new chat (default: the parent's name with " (fork)")
    #[arg(long)]
    pub title: Option<String>,
    /// Work in a new git worktree on a new branch whose files are the checkpoint of the turn
    /// (modified, deleted and untracked files, as uncommitted changes); the branch is taken
    /// from the checkpoint's commit. Without a name: fork/<title>-t<N>. Not for Claude Code
    /// chats unless --mode summary
    #[arg(
        long,
        value_name = "BRANCH",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "files"
    )]
    pub worktree: Option<String>,
    /// First put the files of the chat's folder back to how the turn left them (the restore of
    /// `hutan checkpoint restore`, with its Undo); refused when a file changed in a way the
    /// turns do not explain
    #[arg(long)]
    pub files: bool,
    /// Close the original chat once the fork is up (`hutan chat reopen`-able like any closed
    /// session)
    #[arg(long)]
    pub archive_parent: bool,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// The question
    pub question: String,
    /// Ask Mawas of this workspace: id, unique id prefix, or exact name (default:
    /// $HUTAN_WORKSPACE, else the global Mawas of Home)
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
    /// List the Mawas chats, with their scope and which is the default
    Ls(AssistantLsArgs),
    /// Print the default Mawas chat of a scope, creating it on first use
    Open(AssistantOpenArgs),
    /// Forget the backend chosen for a scope (Mawas Home, setup): it uses the parent
    /// workspace's, the global choice, `[assistant]` of daemon.toml, or the automatic backend
    /// again
    Reset(AssistantResetArgs),
}

#[derive(Debug, Args)]
pub struct AssistantLsArgs {
    /// Only the Mawas chats of this workspace (id, unique id prefix, or exact name)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct AssistantOpenArgs {
    /// The workspace: id, unique id prefix, or exact name (default: $HUTAN_WORKSPACE, else the
    /// global Mawas of Home)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Create one more Mawas chat instead of returning the default
    #[arg(long)]
    pub new: bool,
}

#[derive(Debug, Args)]
pub struct AssistantResetArgs {
    /// The workspace whose own choice is removed (id, unique id prefix, or exact name; default:
    /// $HUTAN_WORKSPACE, else the global Mawas of Home)
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct LspArgs {
    #[command(subcommand)]
    pub command: Option<LspCommand>,
}

#[derive(Debug, Subcommand)]
pub enum LspCommand {
    /// The servers, one line each: state, root, documents, memory (the default)
    Ls,
    /// Like `ls`, with the folder decisions, the limits and what to do about a missing server
    Status,
    /// Start a server again; a missing one is looked for again
    Restart(LspServerArgs),
    /// Stop a server; the next document that needs it starts it again
    Stop(LspServerArgs),
    /// Decide whether language servers may start in a folder (and everything below it). A server
    /// runs the project's build scripts and procedural macros and rust-analyzer does not sandbox
    /// them: trust only folders whose code you would run. Only you can decide, not an agent
    Trust(LspTrustArgs),
}

#[derive(Debug, Args)]
pub struct LspServerArgs {
    /// The server's id as `hutan lsp ls` prints it, or its name when only one server has it
    pub server: String,
}

#[derive(Debug, Args)]
pub struct LspTrustArgs {
    /// The folder (default: the current directory)
    pub folder: Option<PathBuf>,
    /// Never start a server there
    #[arg(long, conflicts_with = "forget")]
    pub deny: bool,
    /// Forget the decision about this folder: it is asked about again
    #[arg(long)]
    pub forget: bool,
}

#[derive(Debug, Args)]
pub struct DiagnosticsArgs {
    /// A file (opened through the daemon, so no window is needed) or a folder (what the servers
    /// know below it); default: the current directory
    pub path: Option<PathBuf>,
    /// Only this serious or more: `error`, `warning`, `info` or `hint`
    #[arg(long, value_name = "LEVEL")]
    pub severity: Option<String>,
    /// For a file: wait at most this many seconds for the server to start and answer (default 30)
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    pub wait: u64,
}

#[derive(Debug, Subcommand)]
pub enum FsCommand {
    /// List a folder: directories first, then files by name; entries a `.gitignore` rule matches
    /// are marked
    Ls(FsLsArgs),
    /// Print a file exactly as it is on disk (byte-order mark and line endings included)
    Cat(FsPathArgs),
    /// What a path is, with the version a later `write --expect` takes
    Stat(FsPathArgs),
    /// Replace a file with the text of stdin, `--from FILE` or `--text`: written to a temporary
    /// file next to it and renamed over it, so a reader never sees half of it; with `--expect`
    /// refused when the file changed since that version (exit 1, the current version on stderr)
    Write(FsWriteArgs),
    /// Find files by a fuzzy name (`rtr` finds `src/router.rs`), best first
    Find(FsFindArgs),
    /// Search file contents: `path:line:text` per hit, honouring `.gitignore`
    Search(FsSearchArgs),
    /// Make folders (never over something that exists; `-p` makes the folders above too and
    /// accepts a folder that is there)
    Mkdir(FsMkdirArgs),
    /// Make empty files; a path that exists is an error (a file is never replaced, and its time
    /// is not touched)
    Touch(FsPathsArgs),
    /// Move or rename: `FROM TO`, or `FROM… FOLDER` (each goes into the folder). Within one
    /// workspace root, never over something that exists; one item failing does not undo the
    /// others
    Mv(FsMoveArgs),
    /// Copy files and folders (recursive, copy-on-write where the volume can): `FROM TO`, or
    /// `FROM… FOLDER`. Never over something that exists; a copy may cross workspace roots
    Cp(FsCopyArgs),
    /// Move paths to the system Trash (nothing is deleted) and print a ticket that
    /// `hutan fs restore` takes
    Trash(FsPathsArgs),
    /// Put back what a trash ticket holds, where it was, never over something that exists
    Restore(FsRestoreArgs),
}

#[derive(Debug, Subcommand)]
pub enum GitCommand {
    /// The commits that reach a revision, newest first: `short-hash date author subject`; for a
    /// file, renames are followed and the old name is shown
    Log(GitLogArgs),
    /// Who last changed each line of a file: `short-hash author date line) text`; lines edited
    /// since the last commit are `Not Committed Yet`
    Blame(GitBlameArgs),
    /// One commit: its message, the files it changed and the diff against its first parent
    Show(GitShowArgs),
}

#[derive(Debug, Args)]
pub struct GitLogArgs {
    /// The repository, a folder or a file in it (default: the current directory)
    pub path: Option<PathBuf>,
    /// Start at this branch, tag or commit (default: HEAD)
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,
    /// At most this many commits (1 to 200)
    #[arg(short = 'n', long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=200))]
    pub limit: u32,
    /// The page after this commit (the hash an earlier answer asked to continue from)
    #[arg(long, value_name = "COMMIT")]
    pub before: Option<String>,
}

#[derive(Debug, Args)]
pub struct GitBlameArgs {
    /// The file
    pub path: PathBuf,
    /// Blame the file as it was at this branch, tag or commit (default: the working tree)
    #[arg(long, value_name = "REV")]
    pub rev: Option<String>,
}

#[derive(Debug, Args)]
pub struct GitShowArgs {
    /// The commit (a hash, a branch, a tag, `HEAD~2`)
    pub rev: String,
    /// Only this file or folder (default: the whole commit, found from the current directory)
    pub path: Option<PathBuf>,
    /// The files with their added and removed lines, without the diff
    #[arg(long)]
    pub stat: bool,
}

#[derive(Debug, Subcommand)]
pub enum TerminalCommand {
    /// Find text in a session's terminal output, scrollback included: `row: line` per match,
    /// oldest first (rows count from the first line the terminal ever scrolled away)
    Search(TerminalSearchArgs),
}

#[derive(Debug, Args)]
pub struct TerminalSearchArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// The text to find (literal; case-insensitive unless --case)
    pub query: String,
    /// QUERY is a regular expression (matched per logical line: soft-wrapped rows are joined)
    #[arg(long)]
    pub regex: bool,
    /// Match case exactly
    #[arg(long)]
    pub case: bool,
    /// At most this many matches (1 to 1000)
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
    /// Start at this row (the number an earlier answer asked to continue from)
    #[arg(long, value_name = "ROW")]
    pub from: Option<u64>,
    /// Only the output of this block (`hutan api terminal.blocks` lists them)
    #[arg(long, value_name = "ID")]
    pub block: Option<u64>,
}

#[derive(Debug, Args)]
pub struct FsLsArgs {
    /// The folder (default: the current directory)
    pub path: Option<PathBuf>,
    /// Do not mark ignored entries
    #[arg(long)]
    pub no_ignore: bool,
    /// At most this many entries (default and ceiling 5000)
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
    /// A path you chose yourself: allowed outside the workspaces (never from an agent's session)
    #[arg(long)]
    pub picked: bool,
}

#[derive(Debug, Args)]
pub struct FsPathArgs {
    pub path: PathBuf,
    /// A path you chose yourself: allowed outside the workspaces (never from an agent's session)
    #[arg(long)]
    pub picked: bool,
}

#[derive(Debug, Args)]
pub struct FsMkdirArgs {
    /// The folders to make
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,
    /// Make the missing folders above too; a folder that exists is fine
    #[arg(short = 'p', long)]
    pub parents: bool,
}

#[derive(Debug, Args)]
pub struct FsPathsArgs {
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct FsMoveArgs {
    /// The sources, then the new path (or the folder they go into)
    #[arg(required = true, num_args = 2.., value_name = "FROM… TO")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct FsCopyArgs {
    /// The sources, then the new path (or the folder they go into)
    #[arg(required = true, num_args = 2.., value_name = "FROM… TO")]
    pub paths: Vec<PathBuf>,
    /// The sources are files you chose yourself: they may lie outside the workspaces (never
    /// from an agent's session); the copy always lands inside one
    #[arg(long)]
    pub picked: bool,
}

#[derive(Debug, Args)]
pub struct FsRestoreArgs {
    /// The ticket `hutan fs trash` printed
    pub ticket: String,
}

#[derive(Debug, Args)]
pub struct FsWriteArgs {
    pub path: PathBuf,
    /// Take the new content from this local file (`-`: stdin, the default)
    #[arg(long, value_name = "FILE", conflicts_with = "text")]
    pub from: Option<PathBuf>,
    /// The new content, as given
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,
    /// The version the content was based on, as `hutan fs stat` prints it (`mtime_ns:size:ino`);
    /// without it an existing file is overwritten whatever it is
    #[arg(long, value_name = "VERSION", conflicts_with = "create")]
    pub expect: Option<String>,
    /// The file must not exist yet (its folder must)
    #[arg(long)]
    pub create: bool,
    /// A path you chose yourself: allowed outside the workspaces (never from an agent's session)
    #[arg(long)]
    pub picked: bool,
}

#[derive(Debug, Args)]
pub struct FsFindArgs {
    pub query: String,
    /// Look below this folder (default: the current directory)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// At most this many (default 50, ceiling 200)
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct FsSearchArgs {
    pub pattern: String,
    /// Search below this folder (default: the current directory)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// The pattern is a regular expression (default: literal text)
    #[arg(long)]
    pub regex: bool,
    /// Ignore case (default: smart: exact when the pattern has a capital letter)
    #[arg(short = 'i', long, conflicts_with = "case_sensitive")]
    pub ignore_case: bool,
    /// Match case exactly
    #[arg(short = 's', long)]
    pub case_sensitive: bool,
    /// Only files matching this glob, relative to the root (`*.rs`); `!glob` leaves them out;
    /// repeatable
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    pub globs: Vec<String>,
    /// At most this many hits (default 200, ceiling 1000)
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// A file as `path`, `path:line` or `path:line:col`
    pub target: String,
}

#[derive(Debug, Subcommand)]
pub enum GrantsCommand {
    /// List the grants: the tool, where it applies (a chat, or every chat of a workspace) and
    /// when it was granted
    Ls(GrantsLsArgs),
    /// Remove one grant (the next request it covered asks again), or all of them
    Revoke(GrantsRevokeArgs),
}

#[derive(Debug, Subcommand)]
pub enum HooksCommand {
    /// List the hooks of daemon.toml with what they ran on, when they last ran, and how it went
    List,
    /// Run one hook now with a sample payload of its event and show what it did (exit code 1
    /// when it did not end well); filters and `when_focused` do not apply
    Test(HooksTestArgs),
    /// The finished runs, newest first, with what each printed (secrets masked)
    Log(HooksLogArgs),
}

#[derive(Debug, Args)]
pub struct HooksTestArgs {
    /// The hook: its number in `hutan hooks list`, or its `name`
    #[arg(value_name = "HOOK")]
    pub hook: String,
}

#[derive(Debug, Args)]
pub struct HooksLogArgs {
    /// Only this hook's runs: its number in `hutan hooks list`, or its `name`
    #[arg(value_name = "HOOK")]
    pub hook: Option<String>,
    /// At most this many runs
    #[arg(long, short = 'n', default_value_t = 20, value_name = "N")]
    pub limit: u32,
}

#[derive(Debug, Args)]
pub struct GrantsLsArgs {
    /// Only the grants that apply to this workspace's chats (id, unique id prefix, or exact name)
    #[arg(long = "ws", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct GrantsRevokeArgs {
    /// The grant to remove (see `hutan grants ls`)
    #[arg(
        value_name = "ID",
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    pub id: Option<u64>,
    /// Remove every grant
    #[arg(long)]
    pub all: bool,
}

/// What a memory entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MemoryCategoryArg {
    /// About you: communication preferences and personal facts you approved
    Profile,
    /// A stable fact about an environment or a project
    Fact,
    /// A decision or working note of one workspace
    Note,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// List the entries (newest change first in each category): id, scope, category, revision,
    /// whether a model wrote it and you have not confirmed it yet (inferred), and the text
    Ls(MemoryLsArgs),
    /// Remember something: a global entry, or with --workspace one of a workspace
    Add(MemoryAddArgs),
    /// Replace an entry's text (saving the same text confirms an inferred entry)
    Edit(MemoryEditArgs),
    /// Stop using an entry; its history is kept
    Archive(MemoryIdArgs),
    /// Use an archived entry again
    Restore(MemoryIdArgs),
    /// Every revision of an entry, newest first, with who wrote it
    History(MemoryIdArgs),
    /// The proposals agents made, waiting for you; --approve or --reject answers one
    Proposals(MemoryProposalsArgs),
    /// Write the memory of a scope to a file (Markdown as the memory files are written, or JSON)
    Export(MemoryExportArgs),
    /// Read a file of entries (Markdown or JSON, as `export` writes them) as proposals you
    /// approve; nothing is saved by the import itself
    Import(MemoryImportArgs),
    /// Where the memory files are, whether they are in a git repository, what waits and which
    /// edits of them could not be used
    Files(MemoryFilesArgs),
}

/// A memory scope: Global, or one workspace (`--ws`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MemoryScopeArg {
    /// Everything that is not about one workspace (the default)
    Global,
    /// One workspace's memory: needs --ws
    Workspace,
}

/// The text format of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MemoryFormatArg {
    /// Markdown, as the memory files are written
    Md,
    /// JSON (`{"format": "hutan-memory", "entries": [...]}`)
    Json,
}

#[derive(Debug, Args)]
pub struct MemoryExportArgs {
    /// Where to write it; `-` is standard output
    pub path: PathBuf,
    /// The scope (default: global; `workspace` needs --ws)
    #[arg(long, value_enum)]
    pub scope: Option<MemoryScopeArg>,
    /// The workspace (id, unique id prefix, or exact name); implies `--scope workspace`
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// The text format (default: by the file's extension, else md)
    #[arg(long, value_enum)]
    pub format: Option<MemoryFormatArg>,
    /// Replace the file when it exists
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MemoryImportArgs {
    /// The file to read (Markdown or JSON); `-` reads standard input
    pub path: PathBuf,
    /// The scope the entries go to (default: global; `workspace` needs --ws)
    #[arg(long, value_enum)]
    pub scope: Option<MemoryScopeArg>,
    /// The workspace (id, unique id prefix, or exact name); implies `--scope workspace`
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// The category of entries no `# Profile` / `# Facts` / `# Notes` heading precedes
    #[arg(long, value_enum)]
    pub category: Option<MemoryCategoryArg>,
}

#[derive(Debug, Args)]
pub struct MemoryFilesArgs {
    /// The scope (default: global; `workspace` needs --ws)
    #[arg(long, value_enum)]
    pub scope: Option<MemoryScopeArg>,
    /// The workspace (id, unique id prefix, or exact name); implies `--scope workspace`
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct MemoryLsArgs {
    /// Only the global entries and those of this workspace (id, unique id prefix, or exact name)
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
    /// Only this category
    #[arg(long, value_enum)]
    pub category: Option<MemoryCategoryArg>,
    /// Include archived entries
    #[arg(long)]
    pub archived: bool,
}

#[derive(Debug, Args)]
pub struct MemoryAddArgs {
    /// What to remember (at most 2 KiB; credentials are refused)
    pub text: String,
    /// The category (default: note with --workspace, else profile)
    #[arg(long, value_enum)]
    pub category: Option<MemoryCategoryArg>,
    /// Remember it for this workspace only (id, unique id prefix, or exact name)
    #[arg(long = "ws", visible_alias = "workspace", value_name = "ID|NAME")]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct MemoryEditArgs {
    /// The entry (id or unique id prefix of at least 6 characters; see `hutan memory ls`)
    pub id: String,
    /// The new text
    pub text: String,
}

#[derive(Debug, Args)]
pub struct MemoryIdArgs {
    /// The entry (id or unique id prefix of at least 6 characters; see `hutan memory ls`)
    pub id: String,
}

#[derive(Debug, Args)]
pub struct MemoryProposalsArgs {
    /// Approve this proposal (id or unique id prefix): it is applied exactly as listed
    #[arg(long, value_name = "ID", conflicts_with = "reject")]
    pub approve: Option<String>,
    /// Reject this proposal (id or unique id prefix): nothing is saved
    #[arg(long, value_name = "ID")]
    pub reject: Option<String>,
    /// Also list the answered ones
    #[arg(long, conflicts_with_all = ["approve", "reject"])]
    pub all: bool,
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

fn parse_safety(text: &str) -> Result<SafetyPreset, String> {
    text.parse()
}

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// List the hosts of settings.toml
    #[command(visible_alias = "ls")]
    List,
    /// Add a host: a name and what `ssh` is given (user@host, a ~/.ssh/config alias, ssh://…).
    /// Keys, jump hosts and options stay in ~/.ssh/config; Hutan stores no credential
    Add {
        /// Name for `--host` (letters, digits, `.`, `_`, `-`)
        name: String,
        /// The ssh destination
        ssh: String,
        /// `hutan` on the host (a path; default: `hutan` from the host's non-interactive PATH)
        #[arg(long, value_name = "PATH")]
        hutan: Option<String>,
    },
    /// Remove a host from settings.toml
    #[command(visible_alias = "rm")]
    Remove { name: String },
    /// Connect to a host and report what answers: time to connect, the daemon's version and
    /// protocol, its round trip
    Test { name: String },
    /// Forward a local Unix socket to the host's daemon socket (`ssh -N -L`) for tools that
    /// speak the protocol themselves; runs until interrupted and restarts ssh when it exits
    Forward {
        name: String,
        /// The local socket to create (default: <hutan home>/hosts/NAME.sock)
        #[arg(long, value_name = "PATH")]
        listen: Option<PathBuf>,
        /// The daemon's socket on the host (default: asked from the host's `hutan`)
        #[arg(long, value_name = "PATH")]
        remote_socket: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start hutand detached and wait for its socket; does nothing if it already runs
    Start,
    /// Show whether hutand runs, and its pid, uptime, and counts; when it does not, why its
    /// last start failed
    Status,
    /// Why hutand is not running: the record of its last failed start, the end of what it
    /// printed while starting, and the end of its log
    Why,
    /// Start with empty data: move the database (and its -wal/-shm files) into backups/ next
    /// to it, so the next hutand starts with none. Nothing is deleted. Refused while hutand runs
    ResetData {
        /// Do it (without it nothing is moved)
        #[arg(long)]
        yes: bool,
    },
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
    /// Put a folder under version control (`git init`, a `.gitignore` with Hutan's scratch
    /// files, an initial commit with your git identity): a project's folder, or the Home folder
    /// with `--home` (where Mawas works). Refused for the home directory, `/`, a folder that is
    /// a repository or inside one, and a folder with more files than checkpoints seed
    GitInit {
        #[arg(help = PROJECT_TARGET_HELP, conflicts_with = "home", required_unless_present = "home")]
        target: Option<String>,
        /// The Home folder (`~/Hutan`) instead of a project
        #[arg(long)]
        home: bool,
        /// Do it without asking (required when stdin is not a terminal)
        #[arg(long, short = 'y')]
        yes: bool,
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
    /// Run the session inside Hutan's Seatbelt boundary: it can write only in its worktree (and
    /// the repository's git directory, the temp folders and --write folders) and still reads
    /// your files. Refused, never started uncontained, when the boundary cannot be applied
    #[arg(long)]
    pub contain: bool,
    /// With --contain: whether the session may use the network (default: `[sandbox] network` of
    /// daemon.toml, on); `off` also closes every unix socket, the daemon's included
    #[arg(long, value_name = "on|off", requires = "contain", value_parser = parse_on_off)]
    pub network: Option<bool>,
    /// With --contain: a folder the session may also write; repeatable
    #[arg(long = "write", value_name = "PATH", requires = "contain")]
    pub write: Vec<PathBuf>,
    /// A further folder the program works in besides --cwd; repeatable, at most 8. A claude,
    /// codex or omp command is started with `--add-dir=<path>` for each; with --contain they
    /// are writable too (also `hutan chat dirs add` later)
    #[arg(long = "dir", value_name = "PATH")]
    pub dirs: Vec<PathBuf>,
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

/// `hutan agents`: the agents (and subagents) of the sessions, as the daemon observes them; with
/// `--board`, every agent in one board; with `--activity`, what happened in a window.
#[derive(Debug, Args)]
pub struct AgentsArgs {
    /// Only this session: id, unique id prefix, or title
    #[arg(value_name = "SESSION")]
    pub target: Option<String>,
    /// Keep running and print the trees again whenever one changes
    #[arg(long, conflicts_with = "board")]
    pub watch: bool,
    /// The board instead of the trees: every agent across workspaces grouped like the Agents
    /// page (what it does, what it waits for, where its facts come from), from the daemon
    #[arg(long, conflicts_with_all = ["target", "watch", "activity"])]
    pub board: bool,
    /// What happened instead: the catch-up across agents (turns, requests and how long they
    /// waited, files changed, tasks, sessions), from what the daemon records; says what it does
    /// not record
    #[arg(long, conflicts_with_all = ["target", "watch"])]
    pub activity: bool,
    /// With --activity: how far back, as a number and a unit (`90s`, `30m`, `2h`, `1d`);
    /// default 24h
    #[arg(long, value_name = "AGO", requires = "activity")]
    pub since: Option<String>,
    /// With --activity: the most events to list (1 to 200; default 100)
    #[arg(long, value_name = "N", requires = "activity")]
    pub limit: Option<u32>,
    /// With --board or --activity: only this workspace or project (name, id or unique prefix)
    #[arg(long, value_name = "WORKSPACE")]
    pub ws: Option<String>,
    /// With --board: list the agents that have finished too
    #[arg(long, requires = "board")]
    pub finished: bool,
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

/// `hutan close`.
#[derive(Debug, Args)]
pub struct CloseArgs {
    #[arg(help = TARGET_HELP)]
    pub target: String,
    /// Stop what runs in the session (a command, a working agent, a chat turn) and close it
    #[arg(long)]
    pub force: bool,
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

/// Color and icon theme packs (`docs/themes.md`): files in `$HUTAN_HOME` (else `~/Library/
/// Application Support/Hutan`) read and written locally; no daemon is started or contacted.
/// Exit status: 0 ok (warnings included), 1 errors in a pack or a file that cannot be read or
/// written, 2 invalid invocation; `validate --strict` makes warnings status 1.
#[derive(Debug, Subcommand)]
pub enum ThemeCommand {
    /// List the installed packs (embedded, yours, and the invalid ones with their errors)
    Ls(ThemeLsArgs),
    /// Check a pack by id or by path with the rules the desktop uses: manifest, inherited
    /// values, assets, contrast and license notes
    Validate(ThemeValidateArgs),
    /// Write a new sparse pack that extends a built-in; refuses to replace an existing one
    New(ThemeNewArgs),
    /// Write an installed pack as an editable native pack into a folder: declarations, assets
    /// and license notices
    Export(ThemeExportArgs),
}

/// Which kind of pack a `hutan theme` command works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ThemeKind {
    /// Chrome, terminal, state and diff colors, fonts, density (`theme.toml`)
    #[default]
    Color,
    /// Monochrome product icons (`icon-theme.toml` and SVG files)
    Icon,
}

#[derive(Debug, Args)]
pub struct ThemeLsArgs {
    #[arg(long, value_enum, default_value_t)]
    pub kind: ThemeKind,
}

#[derive(Debug, Args)]
pub struct ThemeValidateArgs {
    /// A pack id (installed, or embedded), or the path of a pack folder or single-file color
    /// pack (anything with a `/`, or starting with `.`, or ending in `.toml`)
    pub target: String,
    #[arg(long, value_enum, default_value_t)]
    pub kind: ThemeKind,
    /// Warnings fail the command too (exit 1); they never change how the pack renders
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ThemeNewArgs {
    /// The pack id: lower-case words joined by `-`, e.g. `canopy-night`
    pub id: String,
    /// Display name
    #[arg(long)]
    pub name: String,
    /// Author (written to `authors`)
    #[arg(long)]
    pub author: String,
    /// SPDX expression or `LicenseRef-<name>`, e.g. `MIT OR Apache-2.0`
    #[arg(long)]
    pub license: String,
    /// The built-in to extend: `builtin:hutan-dark` (default) or `builtin:hutan-light` for a
    /// color theme, `builtin:phosphor` for an icon theme
    #[arg(long, value_name = "BUILTIN")]
    pub extends: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pub kind: ThemeKind,
    /// Folder the pack folder `<id>` is created in (default: `themes/` or `icon-themes/` of the
    /// data folder)
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ThemeExportArgs {
    /// The id of an installed pack (embedded ones included)
    pub id: String,
    /// Folder the pack folder `<id>` is created in; it must not exist yet
    #[arg(long, value_name = "DIR")]
    pub output: PathBuf,
    #[arg(long, value_enum, default_value_t)]
    pub kind: ThemeKind,
    /// Write a self-contained `root = true` pack with every fallback, base value and (for a
    /// color pack) your overrides of it written out
    #[arg(long)]
    pub resolved: bool,
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

fn parse_on_off(text: &str) -> Result<bool, String> {
    match text {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("expected `on` or `off`, got `{other}`")),
    }
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => Err(format!("expected K=V, got `{text}`")),
    }
}
