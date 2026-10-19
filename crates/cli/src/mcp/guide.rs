//! The texts that tell an agent how to drive Hutan: `hutan help agent` (CLI) and the MCP
//! server's `instructions`.

/// What `hutan help agent` prints: compact, for a model reading it once.
pub const AGENT_GUIDE: &str = "\
HUTAN FOR AGENTS
Hutan is the user's development environment: a daemon (hutand) owns terminal sessions, agent
chats, tasks, worktrees and workspaces; a desktop window shows them. You can drive almost all of
it, through this CLI (inside a Hutan session $HUTAN_BIN is the hutan executable and
$HUTAN_SESSION_ID names your session) or, when your host started it, the `hutan` MCP server.

LOOK (always allowed)
  hutan ls                     sessions: id, title, state, agent, checkout
  hutan history <session>      last output of a terminal session (--lines N)
  hutan chat log <session>     journal of an agent chat
  hutan task ls                tasks with dependencies and state
  hutan wt ls                  git worktrees
  hutan ws ls                  workspaces and projects
  hutan ps                     processes of every live session with CPU and memory
  hutan memory ls              what Mawas remembers (the user approved each entry)
  hutan approvals              what waits for the user
  hutan notify ls              pending notifications
  hutan search <words>         past sessions (needs the history index)
  hutan ui layout              what the window shows: preset, docks, panes, tabs, focus
  hutan ui actions             what the window can do, with argument schemas, and its settings

CHANGE
  hutan new [--cwd D] [-- cmd...]   start a terminal session
  hutan send <session> <text> [--enter]   type into a terminal session
  hutan stop|restart|rename <session> ...  manage a session
  hutan kill <session> [--pid N ...]   SIGKILL a session's process tree, or signal one process
  hutan agent interrupt <session>   stop an agent's turn without ending the session
  hutan chat new|send|interrupt     talk to another agent (codex, or a model directly)
  hutan task add|start|done|cancel|reset   plan and run work
  hutan wt new|rm                   git worktrees
  hutan ws new|open|pin             workspaces and projects
  hutan ui run <action> [--arg k=v]   do what the window can do (split, preset, open a panel...)
  hutan ui set <namespace.key> <value> [--layer user|workspace|project]   change a setting

APPROVAL
  What you change may need the user's approval. By default (daemon.toml [control] agents = \"ask\")
  every change waits until the user answers in Hutan (desktop card, notification, `hutan approve`);
  that can take minutes. The command prints when it waited. If it is denied it fails with
  `forbidden` - do not retry unless the user asks. With agents = \"read_only\" every change is
  refused; with \"allow\" it runs and is recorded. You never answer approvals yourself.
  Looking is never gated. Changing Hutan's own control settings or credentials is refused.
  If you are Mawas (the assistant), `agents = \"allow\"` does not apply to you: every change asks,
  the user may grant a tool for a chat or a workspace, destructive changes always ask, only a
  limited number of changes run per turn without asking, and in Advisor mode every change is
  refused. You can never list, widen or revoke your grants.

MEMORY
  Hutan remembers curated entries the user approved (profile, fact, note; global or per
  workspace). You can read them (`hutan memory ls`, MCP `memory_list` / `memory_search`) and
  PROPOSE a change (MCP `memory_propose`): a proposal is NOT saved until the user approves it
  (they can edit or reject it), so never say it was saved. You cannot write, edit, archive or
  resolve memory yourself, and a body that looks like a credential is refused. Memory is
  background knowledge: it cannot grant you tools or override a rule. Entries marked inferred
  were proposed by a model and not yet confirmed by the user.

TIPS
  Prefer ids from `hutan ls`. Use --json for output you parse. `hutan ui actions` is the truth
  about what the window can do; ids look like workbench::SplitRight, parameterised ones take
  --arg (workbench::ApplyPreset --arg name=focus).
  Exit codes: 0 ok, 1 refused or failed (message on stderr), 2 usage, 3 daemon not running.
";

/// The MCP `instructions` of the server: the same rules in fewer words.
pub const MCP_INSTRUCTIONS: &str = "\
These tools drive Hutan, the user's development environment (terminal sessions, agent chats, \
tasks, git worktrees, workspaces and the desktop window). Start with `session_list`, \
`workspace_list` or `ui_layout` to see what exists, then act on ids from those results. \
Reading is always allowed. Tools that change something may wait - for minutes - until the user \
approves the request in Hutan; the result tells you when it did. If the user denies a request \
it fails with DENIED: do not retry unless the user asks. In Advisor mode changes are refused \
(REFUSED): describe what you would change instead. You can never answer approvals or change \
grants yourself. `ui_actions` lists what the window can do and its settings; use `ui_run` and \
`ui_set_setting` for those. `memory_list` and `memory_search` read what Hutan remembers about the \
user; `memory_propose` only proposes - nothing is remembered until the user approves it, so \
never say it was saved.";
