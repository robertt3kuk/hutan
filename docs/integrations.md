# Agent integrations

Coding agents running in a Hutan terminal session tell the daemon what they are doing and how to start them again. The daemon shows the state on the session (`SessionInfo.agent`, protocol 0.4, capability `agent.report`) and, after a daemon restart, runs the reported command instead of the session's original one, so a restart finds Codex, Claude Code, and OMP in the same conversation. The monitor of M13 builds on the same data.

Wire contract and daemon semantics: `docs/protocol.md` § Agent reporting. This file is about the pieces on the agent side.

## What a session process gets

Every session process runs with `HUTAN_ENV=1`, `HUTAN_SESSION_ID`, `HUTAN_WORKSPACE_ID`, `HUTAN_SOCKET`, and `HUTAN_BIN` (the `hutan` CLI). Anything that runs inside the session — an agent, one of its hooks, an extension — can report without configuration.

## Reporting from the command line

```sh
hutan agent report --source my-hooks --agent omp --state working \
    [--seq N] [--message TEXT] [--agent-session-id ID] [-- omp --resume=ID]
hutan agent release --source my-hooks
```

- `--state` is `idle`, `working` (a turn is running) or `blocked` (waiting for a permission or an answer).
- Everything after `--` is the **resume command**: a plain command name on `PATH` (no path, so it keeps working when binaries move), at most 64 words and 8 KiB, no apostrophes or control characters. The daemon runs it in place of the session's command after a restart. Report it as early as the agent knows its conversation id, and again whenever the id changes.
- `--seq` must increase per `(session, source)`; the default is the current time in nanoseconds. Pass your own when reports are produced by one process and delivered by several.
- `release` says the agent is gone. The daemon also forgets the agent when the session's process ends and when the shell prompts again (OSC 133 `A` from the shell integration, `docs/shell-integration.md`).

Both commands are built to be called from hooks: outside Hutan (no `HUTAN_SESSION_ID`), without a daemon, on any daemon error, or after 200 ms they do nothing, print nothing, and exit 0. They never start a daemon.

`hutan agent hook <codex|claude>` is the same for hook events: it reads the hook's JSON from stdin and turns it into the report below. The installed hook scripts call it.

## Installing the integrations

```sh
hutan integration install   codex|claude|omp [--dir DIR]
hutan integration status    codex|claude|omp [--dir DIR]
hutan integration uninstall codex|claude|omp [--dir DIR]
```

The agent's configuration directory is `--dir`, else `$CODEX_HOME` / `$CLAUDE_CONFIG_DIR` / `$PI_CODING_AGENT_DIR`, else `~/.codex` / `~/.claude` / `~/.omp/agent`. `--json` prints the report as JSON.

What `install` does, and only this:

- Writes Hutan's own file: `<codex dir>/hutan-agent-state.sh`, `<claude dir>/hooks/hutan-agent-state.sh` (the same script, `integrations/hooks/`), or `<omp dir>/extensions/hutan-agent-state.ts` (`integrations/omp/`). Files carry a `hutan-managed` marker in their first lines; a file of that name without the marker is never overwritten or removed.
- Codex and Claude Code: adds one hook group per event to `hooks.json` / `settings.json`, running `: hutan-integration=<agent>; sh '<script>' <agent>`. The marker in the command is how Hutan finds its entries. Every other key, group and hook — including those of Herdr and Orca — stays; the document keeps its key order and indentation (a file that was written by Node or by Hutan comes back byte for byte after an uninstall).
- Before it changes an existing configuration file it copies it to `<name>.hutan-<unix ms>.bak`. A file that is not valid JSON, or not shaped like the agent documents, is left alone and the install fails.
- Idempotent: a second install changes nothing and writes no backup. An install of a newer version updates its own entries and script in place.

`uninstall` removes exactly those hooks (and the groups, events, and `hooks` table they leave empty), the script, and the extension. `status` says `installed`, `partial` (with what is missing or outdated) or `not installed`.

Codex asks you to review new or changed hooks the first time it starts after an install (its `hooks.state` trust list in `config.toml`); Hutan does not touch that file, so approve the Hutan hooks there once.

### Installing on launch (`integrations.auto_install`, off by default)

Hutan never writes into an agent's configuration on its own. The setup's Agents step offers **Install hooks for OMP / Claude Code / Codex** as unticked checkboxes (what each writes and where) and installs only the ticked ones when the step is left; `HUTAN_HOME` does not move these folders, and the step says so when it is set. Turning on `integrations.auto_install` (the step's *Keep hooks installed on every start*, or Settings) makes the desktop run `hutan integration install <omp|claude|codex>` itself (the `hutan` beside the app: `Contents/MacOS/hutan` in the bundle, `target/debug/hutan` in development) when it starts and the daemon answers, off the main thread, for each harness the Accounts page finds installed whose `status` is not `installed`. The installs are idempotent, only touch Hutan-marked entries and do nothing outside a Hutan session. Results go to the log; a failure shows one notice with the command to run by hand. `integrations.auto_install = false` (the default, also for a setup that never chose) installs nothing by itself and does not uninstall (the setup's Agents step has *Uninstall hooks* per harness, and Settings → Agent Integrations shows the status of each agent's hooks — installed, outdated or incomplete, not installed — with *Install hooks…*, *Repair hooks…* and *Uninstall hooks…*, each confirming with the files it changes; all are `hutan integration install|uninstall`). Codex asks once to trust new hooks (above).

## Letting an agent drive Hutan: `--mcp`

```sh
hutan integration install   codex|claude|omp --mcp [--dir DIR]
hutan integration status    codex|claude|omp --mcp [--dir DIR]
hutan integration uninstall codex|claude|omp --mcp [--dir DIR]
```

With `--mcp` the command manages the `hutan mcp` server (`docs/control.md`) in the agent's own MCP configuration, **instead of** the hooks; without it no server entry is written or removed. The directory is the same as for the hooks.

- Where: `[mcp_servers.hutan]` in Codex's `config.toml`; `mcpServers.hutan` in Claude Code's `.claude.json` (next to `~/.claude/`, or inside `$CLAUDE_CONFIG_DIR` / `--dir` when that is not a `.claude` directory) and in OMP's `mcp.json`. The entry runs the `hutan` executable with `mcp`, and asks the host to wait up to 700 s for a tool (an approval can take minutes).
- Only Hutan's entry: it is marked by `env.HUTAN_MCP_ENTRY = "hutan-managed"`. A `hutan` server without the marker is never overwritten or removed (the install fails and says so; nothing changes). Every other key and server stays; a TOML file keeps its comments.
- The file is copied to `<name>.hutan-<unix ms>.bak` before it changes; a file that does not parse is left alone and the command fails. Idempotent: a second install changes nothing and writes no backup. `uninstall` removes the entry and the table or file it leaves empty. `status` says `installed`, `partial` (the entry runs another `hutan` than this one) or `not installed`.
- Hutan never writes these files by itself, and the tests use temporary directories only. The OMP and Claude Code locations follow their documentation and OMP's schema; they were not checked against a live install.

Agents Hutan starts itself (Codex chats, ACP harnesses) get the server without any of this, while `[control] mcp = true`. Whether the agent's requests are gated depends on where it runs: see `docs/control.md` § Who is asking — in short, an agent that reported itself in a terminal (the hooks above) or a chat session is gated by `[control]`; a plain shell, and a server started outside any Hutan session, is the user.

## What each integration reports

| Agent | Event | Report | Resume command |
|---|---|---|---|
| Codex | `SessionStart` | idle | `codex resume <session_id>` |
| | `UserPromptSubmit`, `PostToolUse` | working | same |
| | `PermissionRequest` | blocked, `permission: <tool>` | same |
| | `Stop` | idle | same |
| Claude Code | `SessionStart` | idle | `claude --resume <session_id>` |
| | `UserPromptSubmit`, `PostToolUse` | working | same |
| | `PermissionRequest` | blocked, `permission: <tool>` | same |
| | `Stop` | idle (a normal stop: raises `agent_finished` after a long turn) | same |
| | `StopFailure` | idle + outcome `failed` on the root (no `agent_finished`); the next prompt starts it again | idle |
| | `PreToolUse` | the tool as the node\'s activity (`Bash: <command>`); runs in the background | same |
| | `SubagentStart` / `SubagentStop` | a subagent node keyed by `agent_id`: working / idle + completed (`agent_transcript_path` kept) | — |
| | any tool or permission hook with an `agent_id` | that subagent\'s activity, working or blocked | — |
| | `PostModelSwitch` | the main agent\'s model | — |
| | `SessionEnd` | release | — |
| OMP | `session_start`, `session_switch` | idle, node `ctx.agent.id` (kind, parent, name, model from `ctx`) | `omp --resume=<id>` |
| | `agent_start` | working | same |
| | `tool_approval_requested`, the `ask` tool | blocked, the reason or question | same |
| | `agent_end` | idle (after 400 ms, so an automatic retry stays working; `willContinue` keeps it working) | same |
| | the same events in a subagent session | the subagent\'s node under `ctx.agent.parentId`; its end is idle + completed | — |

Sources are `codex-hooks`, `claude-hooks`, `omp-extension`. `PostToolUse` is hooked because hooks never say that a permission prompt was answered; the next tool result is the signal. Hook events of a Codex whose `CODEX_THREAD_ID` is another conversation (a Codex started by Codex's own tools) are ignored. Session ids that are not made of letters, digits, `-_.:` are never put in a resume command (the state is still reported). The Claude hooks do not install a status line (it would replace the user's); Claude's `subagentStatusLine` (per-subagent model and token counts) is a possible later source.

Resume commands are what the agents document: `codex resume <id>`, `claude --resume <id>`, `omp --resume=<id>`. For other agents (`opencode --session <id>`, …) call `hutan agent report` from their own hooks.

A restart that runs a resume command says so on the session (`recovery.kind = agent_resumed` with the argv; `hutan ls` shows `resumed`): the screen and the agent's process of the previous run are gone, the agent continues its own conversation. See `docs/protocol.md` § Recovery.

## Pasting images (`terminal.paste_images`)

⌘V of a screenshot or *Copy Image* in a Hutan terminal saves the image as a PNG in the session's scratch folder (`$HUTAN_HOME/scratch/<session>/paste-<n>.png`, removed by the daemon when the session is closed) and pastes the file's **path** — the one thing a program in a terminal can be given, and what the coding agents take: they read an image file named in their prompt. `docs/design.md` § Terminal drops and pastes has the limits, the toast and the pruning; this is what each agent does with the path.

The terminal knows which agent runs in it from the integration state above (`SessionInfo.agent.agent`: `claude`, `codex` or `omp`, set by the hooks / extension; nothing else counts). With one of them the path is typed **backslash-escaped** (`/Users/me/Library/Application\ Support/Hutan/scratch/…/paste-1.png `); in a plain shell, or under an agent that did not report, it is single-quoted where a shell would care, like a drop. None gets an `@` or any other prefix: the plain path is what each documents or accepts, and the escaped form is the only one all three read back as a single path.

| Agent | What it does with a pasted path | Checked against |
|---|---|---|
| Claude Code | a path to an image in the prompt (typed, pasted, or dragged: the terminal types a shell-escaped path) becomes an image attachment; the docs list "provide an image path" next to drag and drop and `Ctrl+V` | `code.claude.com/docs/en/common-workflows` § Work with images; a public report of shell-escaped drags attaching (anthropics/claude-code#80980). Not run against a live Claude Code. |
| Codex | a pasted text that is one path (`file://`, quoted, or shell-escaped, parsed with `shlex`) to an image file attaches it, unless the composer's `image_paste_enabled` is off; `codex -i` is the command-line form | `codex-rs/tui/src/clipboard_paste.rs` (`normalize_pasted_path`) and `bottom_pane/chat_composer.rs` in openai/codex `main`. Not run against a live Codex. |
| OMP | a bracketed paste that is only image paths (quoted or backslash-escaped; several allowed) becomes image attachments (`extractBracketedImagePastePaths`); a path with an **unescaped** space does not (can1357/oh-my-pi#6578), which is why the path is escaped. It needs the paste to be bracketed, which Hutan sends exactly when the program turned that mode on | the strings of the installed OMP 18.12.3 binary (the path-extraction functions and their quoting rules). Not run against a live OMP. |

What was not verified: Hutan did not drive any of the three with a pasted path. The tests prove what reaches the terminal (`crates/terminal_view/tests/it/view.rs`, `crates/desktop/tests/it/term_image_paste_gpui.rs`: a real `cat` in a PTY echoes the path), not what an agent does with it. Another agent is a plain shell for this purpose: its path is quoted; `hutan agent report --agent <name>` with one of the three names above is how a script or hook makes a terminal count as that agent.

## Observation tree

Every session has a tree of agents: the main agent and the subagents it spawned, as the daemon observes them (`hutan agents [--json] [--watch]`, protocol `agent.tree`, `docs/protocol.md` § Agent observation tree). Reporters name a node with `hutan agent report --node ID [--parent ID --kind sub --name N --agent-type T --model M --activity A --usage-json '{"kind":"omp.tokens","input":1200}' --outcome completed|failed|interrupted|disconnected]`, and end one with `hutan agent release --node ID [--outcome O]`; without `--node` a report is about the main agent, as before. Sequence numbers count per session, source and node; reports are ordered by `seq`, not arrival. A reporter that runs as a process of the session adds `--pid PID` (protocol 0.18): `hutan ps` marks that process and `hutan agent interrupt SESSION NODE` can signal it (`docs/resources.md`).

- **Claude Code**: subagents are nodes keyed by the hooks' `agent_id` (the main agent is the node `root`); Claude reports no parent, so subagents hang off the main agent.
- **OMP**: the extension runs once per session and per subagent; `ctx.agent` gives `kind`, `id`, `parentId`, `name`.
- **Codex (chats Hutan runs)**: the adapter feeds the tree from `thread/status/changed` (`active` with `waitingOnApproval` / `waitingOnUserInput` is blocked), `thread/started` (`parentThreadId`, `agentNickname`, `agentRole`), `collabAgentToolCall.agentsStates`, `subAgentActivity`, a subagent's `turn/completed`, and `thread/tokenUsage/updated` (usage kind `codex.total`). A finished tool call is not a finished child: only the child's own status ends it. Codex sessions started outside Hutan are not observed.
- **Desktop**: the Agents page and the `agents` panel, the badges on rows and tabs, and the attention rollup draw these trees (`docs/design.md` § Agent monitor).
- **Attention**: a session needs attention when any node is blocked; `SessionInfo.agent` rolls that up for older clients (`blocked_sub`).
- **Lifecycle**: the tree is stored (schema v4) without what the agents are doing right now. After a daemon restart the main agent is `unknown` until it reports; subagents are `disconnected`. When the session's process ends, every unfinished node is `disconnected`.

Not yet: reconstructing trees from OMP directories, Claude `subagents/` files or Codex `thread_spawn_edges` for sessions Hutan did not watch (M13-C).

## Safety

Nothing here runs with more rights than the agent has. The hook script is a few lines of `sh` that checks `HUTAN_ENV`, finds `HUTAN_BIN`, and calls `hutan agent hook`; the daemon validates every resume command before it stores it. No credentials pass through any of it.

Tests: `crates/daemon/tests/it/agent.rs` (protocol semantics, restart resumes a fake agent through its reported argv), `crates/cli/tests/it/{agent,integration,integration_mcp,omp_extension}.rs` (CLI against a real daemon; installs on temporary copies of real-shaped configuration; the OMP extension under Bun, skipped when Bun is missing).
