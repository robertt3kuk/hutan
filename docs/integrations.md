# Agent integrations

Coding agents running in a Hutan terminal session tell the daemon what they are doing and how to start them again. The daemon shows the state on the session (`SessionInfo.agent`, protocol 0.4, capability `agent.report`) and, after a daemon restart, runs the reported command instead of the session's original one, so a restart finds Codex, Claude Code, and OMP in the same conversation. The monitor of M13 builds on the same data.

Wire contract and daemon semantics: `docs/protocol.md` § Agent reporting. This file is about the pieces on the agent side, and (§ User hooks) about the hooks you write yourself.

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

## Agents that report in the terminal (AGENT-TERM-1, protocol 0.55)

A coding agent in a terminal session needs no installed hook to be seen. The daemon recognises it from three things and publishes the result as `SessionInfo.terminal_agent` (`docs/protocol.md` § Terminal agents): `{app, state, kind?, message?, since_ms, title?, source}`.

**OSC 7501, the program status** (what OMP 18.12 writes about itself; the bytes were read from its binary and checked against a real `omp` in a PTY — the recorded start is in the terminal corpus, `crates/terminal/tests/corpus/recorded/omp_startup.raw`):

```
ESC ] 7501 ; state=<working|blocked|done|error|idle> [:kind=<kind>] :app=<name> [:msg=<base64>] ESC \
ESC ] 7501 ; state=clear ESC \
```

| `state` | meaning | what Hutan does |
|---|---|---|
| `idle` | the prompt is up | agent known, waiting |
| `working` | a turn runs | working; the tab shows a spinner |
| `blocked` | it waits for you; `kind` says for what, `msg` is the question | a **request**: *Requests* in the sidebar, the tab's bell, the question under the row, `agent_blocked` notification with the question as its text |
| `error` | the turn failed; `msg` says why | **Failed**: the failure glyph on the tab, the message under the row |
| `done` | the turn ended well | idle; a mark on the tab and the row until the session is looked at; a task running in the session finishes |
| `clear` | the agent exited | the agent is forgotten |

The parser is bounded (the whole OSC string at most 8 KiB, `app` 64 and `kind` 32 characters of `A-Za-z0-9._-`, a message 2048 decoded characters, control characters removed) and tolerant (unknown keys and fields in any order; a bad field loses only itself, an unknown `state` loses the report). The message is redacted like every agent text (`hutan_index::redact`) and cut to 512 characters by the daemon.

**The program in the foreground.** Shortly after a terminal starts and after every prompt or command mark of the shell (OSC 133 `A` / `C`; three looks: 0.4 s, 1.6 s, 4 s after the last mark) the daemon looks at the terminal's process tree for a known harness — `omp`, `claude`, `codex`, `gemini`, `opencode`, `aider` — by executable or by the script a runtime runs (`node …/claude-code/cli.js`, `npx claude`, `env X=1 omp`). So the icon shows before the first report, and for harnesses that report nothing in the terminal. Such an agent has `state: unknown` — Hutan does not invent a state — except that a braille spinner at the start of its terminal title reads as `working` and the first plain title after one as `idle` (Claude Code's and Codex's titles behave that way).

**The title.** Agents put their session's name in the terminal title behind their own mark and a spinner (OMP: `π ⠋ Fix the build`). The daemon strips them (`π`, braille patterns, `✳`-like asterisks, `>`/`-` separators); what is left is `terminal_agent.title`. A title that only names the program (`Claude Code`) or a directory gives none.

**Prompt marks.** OMP writes OSC 133 marks of its own around every redraw, so a prompt mark does not end an agent that reports itself — its `clear`, or the end of its process, does. If it dies without either, the look that follows the shell's next prompt finds no program of it and nothing running below the shell, and drops it. A program-only agent (Claude Code, Codex) is dropped as soon as the shell prompts.

**Naming.** A session nobody named is called by its agent's title (`docs/design.md` § Session names); the raw title never is, since its spinner changes every frame. A name the user chose stays, and the sidebar shows the agent's title under it. When the agent leaves (`clear`, the shell prompts again, the process ends) the title it set goes with it.

**`hutan ls`** has an `AGENT` column: `omp working`, `omp blocked: Run the tests?`, `claude` (known by its program only), `-` for none; `hutan ls --json` has the whole `terminal_agent`.

**Hooks and OSC together.** OSC reports are folded into the session's agent tree as the main agent of source `osc7501`, so the summary older clients read (`SessionInfo.agent`), tasks and notification intents follow without a second mechanism: `working`/`blocked` set the node's state, `idle` and `done` leave it idle, `error` finishes it `failed` (the next `working` revives it). When an installed hook already reports the main agent (another source, not finished) the tree is left to it and only `terminal_agent` changes — nothing is counted twice — and the desktop shows the hook's state.

## Observation tree

Every session has a tree of agents: the main agent and the subagents it spawned, as the daemon observes them (`hutan agents [--json] [--watch]`, protocol `agent.tree`, `docs/protocol.md` § Agent observation tree). Reporters name a node with `hutan agent report --node ID [--parent ID --kind sub --name N --agent-type T --model M --activity A --usage-json '{"kind":"omp.tokens","input":1200}' --outcome completed|failed|interrupted|disconnected]`, and end one with `hutan agent release --node ID [--outcome O]`; without `--node` a report is about the main agent, as before. Sequence numbers count per session, source and node; reports are ordered by `seq`, not arrival. A reporter that runs as a process of the session adds `--pid PID` (protocol 0.18): `hutan ps` marks that process and `hutan agent interrupt SESSION NODE` can signal it (`docs/resources.md`).

- **Claude Code**: subagents are nodes keyed by the hooks' `agent_id` (the main agent is the node `root`); Claude reports no parent, so subagents hang off the main agent.
- **OMP**: the extension runs once per session and per subagent; `ctx.agent` gives `kind`, `id`, `parentId`, `name`.
- **Codex (chats Hutan runs)**: the adapter feeds the tree from `thread/status/changed` (`active` with `waitingOnApproval` / `waitingOnUserInput` is blocked), `thread/started` (`parentThreadId`, `agentNickname`, `agentRole`), `collabAgentToolCall.agentsStates`, `subAgentActivity`, a subagent's `turn/completed`, and `thread/tokenUsage/updated` (usage kind `codex.total`). A finished tool call is not a finished child: only the child's own status ends it. Codex sessions started outside Hutan are not observed.
- **Desktop**: the Agents page and the `agents` panel, the badges on rows and tabs, and the attention rollup draw these trees (`docs/design.md` § Agent monitor).
- **Attention**: a session needs attention when any node is blocked; `SessionInfo.agent` rolls that up for older clients (`blocked_sub`).
- **Lifecycle**: the tree is stored (schema v4) without what the agents are doing right now. After a daemon restart the main agent is `unknown` until it reports; subagents are `disconnected`. When the session's process ends, every unfinished node is `disconnected`.

Not yet: reconstructing trees from OMP directories, Claude `subagents/` files or Codex `thread_spawn_edges` for sessions Hutan did not watch (M13-C).

## User hooks

Hutan's own hooks above only feed its state. **User hooks** are yours: commands the daemon runs when something happens in Hutan — a turn ends, an agent asks for a permission, a session exits, a task is done — to notify your phone, play a sound, append to a journal, or start anything else (the need behind [Codex #2109](https://github.com/openai/codex/issues/2109)). Protocol 0.48, capability `hooks` (`docs/protocol.md` § Hooks); code: `crates/daemon/src/hooks.rs`, `crates/protocol/src/hooks.rs`.

### Configuration

`[[hooks]]` tables of the user's **`daemon.toml`** (the daemon's own file in the data directory; there is no separate `hutan.toml`). Hooks are **user-only**: nothing from a repository is ever read (a `.hutan/` folder or a `daemon.toml` in a project does nothing), `hutan config` and `daemon.settings.set` have no key for them, and an agent-originated connection cannot list, test or read the log of a hook, nor write `daemon.toml` through the file service (`docs/control.md`). The file is read when the daemon starts; an invalid entry (an unknown key or event, an empty `run`, a `timeout` outside 1–300) stops the start with a message that names it. At most 64 hooks.

```toml
[[hooks]]
name = "Glass when Claude is done"      # optional label for the lists
on = "turn.finished"                    # required, one of the events below
harness = "claude"                      # optional filters: harness, project, workspace
run = ["afplay", "/System/Library/Sounds/Glass.aiff"]   # argv: the program and its arguments
timeout = 30                            # seconds before it is killed; 1–300, default 30
when_focused = false                    # false: skip while a Hutan window is the one you work in; default true
```

- **`on`**: `turn.finished`, `turn.failed`, `approval.requested`, `attention.changed`, `session.exited`, `task.done`, `finish.done`.
- **Filters** `harness`, `project`, `workspace`: each must equal the event's value, by name or (project, workspace) id, ignoring case. An event without the thing a filter names (a `finish.done` has no harness) does not match a hook that sets it. A project is a folder Hutan tracks; a workspace is a group of projects.
- **`run`**: an argument vector, **run without a shell**: `["curl", "-d", "hi", "https://…"]`. With **`shell = true`** `run` is exactly one command line, run by `/bin/sh -c` (pipes, `$(…)`, `~`); the payload's values are in `HUTAN_EVENT_*` variables, so quote them (`"$HUTAN_EVENT_SESSION_TITLE"`) and never paste event text into the command line yourself.
- **`when_focused = false`** skips an event while a desktop window is active (the one you are typing in). The skip is counted. Without a desktop connected nothing is focused.

### When each event fires

| Event | Fires when | Not when |
|---|---|---|
| `turn.finished` | a chat's turn completes (Codex, ACP harnesses, direct chats); a terminal session's main agent goes from working to idle with a normal stop (the Claude Code / Codex / OMP integrations above) | the turn failed or was interrupted; a subagent ends |
| `turn.failed` | a chat turn fails; a terminal agent's turn ends with outcome `failed` | a cancelled or interrupted turn |
| `approval.requested` | an agent asks for a permission (the same moment as the approval card) | the answer; a question (`attention.changed` covers it) |
| `attention.changed` | a session starts or stops needing you: an agent node is blocked, or an approval or a question waits (`needs_attention` true), and when the last of them is gone or the session ended (false) | a second approval while one already waits |
| `session.exited` | a session's process ended (`state` `exited` with `exit_code`, or `signaled`) | a session you close afterwards |
| `task.done` | a task changes into `done` | any other state; a task edited while done |
| `finish.done` | your Finish of a checkout (`hutan finish`, the Changes view) ended, with `state` `ok` or `failed` | a plain commit or merge |

Muting a session does not stop its hooks (muting is about notifications); the `[notify]` settings do not apply to hooks.

### What a hook gets

The **event payload** as one JSON object plus a newline on **standard input**, and the same facts as **environment variables**: `HUTAN_EVENT` is the event's name and every other scalar field `HUTAN_EVENT_<FIELD>` (`HUTAN_EVENT_SESSION_TITLE`, `HUTAN_EVENT_STATE`, …). The command also gets the login environment (what a terminal tab gets; the daemon's own with `[terminal] env_inherit = "daemon"`), `HUTAN_SOCKET` and `HUTAN_BIN` (so it can call `hutan`), and runs in your home folder. Absent fields are absent from the JSON and have no variable.

| Field | Events | Meaning |
|---|---|---|
| `schema` | all | `1`; changes only when a field changes meaning or goes |
| `event` | all | the event's name |
| `time_ms` | all | when the daemon saw it, milliseconds since the Unix epoch |
| `test` | `hooks.test` | `true` for a sample payload |
| `session_id`, `session_title` | those with a session (`task.done` when the task ran in one) | the session; the title is redacted, one line, at most 120 characters |
| `harness` | those with a session | the agent: `codex`, `omp`, `claude`, `direct`, … (for a terminal agent, the program it reported) |
| `project_id`, `project`, `workspace_id`, `workspace` | when known | the project the session belongs to, and the workspace it is in |
| `state` | all | `completed`/`failed` (turns), `pending` (`approval.requested`), `attention`/`clear`, `exited`/`signaled`, `done`, `ok`/`failed` (`finish.done`) |
| `detail` | `approval.requested` (the approval's title), `task.done` (its outcome, e.g. `exited 0`), `session.exited` (`signal 9`), `finish.done` (the checkout's folder name) | redacted, one line, at most 240 characters |
| `duration_ms` | turns, `task.done` | how long it took, when known |
| `changed_files` | turns of chats, `finish.done` | the number of files the turn reported changed, or the Finish committed; absent when unknown or none |
| `exit_code` | `session.exited` | absent when a signal ended it |
| `needs_attention` | `attention.changed` | whether the session needs you now |
| `approval_id` | `approval.requested` | for `hutan approve` |
| `task_id`, `task_title` | `task.done` | the task; the title is redacted and at most 120 characters |
| `branch`, `target_branch`, `commit_step`, `merge_step`, `push_step` | `finish.done` | the branches, and how each step went: `ok`, `skipped` or `failed` (a step that did not run is absent) |

What is **never** in a payload: transcript or message text, a prompt, the failure message of a turn, an approval's command line (only its title), a diff, a file path or working directory (a Finish gives the checkout's folder *name*), an environment or a credential. Text fields go through the same redaction as notifications, are cut after redaction, and are single lines. The payload is meant to be safe to hand to a notification service; whether to send it is still your file's decision.

### Running, limits and the log

- **One run per hook at a time, four at once over all hooks.** Each hook has its own queue of **8** waiting events; an event that finds it full is **dropped and counted** (`dropped`), and the daemon's event loop never waits for a command. A slow hook therefore loses its own events, not the others' time.
- **Timeout:** past `timeout` the command **and every process it started** are killed (`timed_out` in the log). A command that ignores its input is fine.
- **Output:** standard output and error are read (so a chatty command cannot block), redacted, and kept up to **4 KiB each** per run in a ring of the last **200 runs**, the *Hooks log*. The log is in memory and ends with the daemon; each run is also one `info` line of the daemon's log (target `hutan::hooks`). The exit code and duration are recorded; `0` is `ok`.
- A command that does not exist fails as `spawn_failed` with the reason in the log.

### Seeing and testing hooks

- **Settings → Automation** lists each hook with what it runs on, its command, its last run (exit, duration, how long ago, what it printed), its counters (runs, failed, dropped, skipped) and **Run test** (runs it now with a sample payload of its event and shows how it went, ignoring filters and `when_focused`), and below them the log.
- The page also says that hooks are read **when the daemon starts**, and has **Open daemon.toml** (the editor the system opens it with) and **Restart daemon…** (the same confirmation as the Daemon menu) beside that note, so a hook you just wrote is one click from running.
- **`hutan hooks list`** is the table (hooks are numbered from 1; `--json` is the daemon's answer). **`hutan hooks test <N|name>`** runs one with a sample payload, prints the result and the payload, and exits 1 when it did not end `ok`. **`hutan hooks log [N|name] [-n COUNT]`** prints the runs, newest first. All three are refused inside an agent's session (`only available to the user`).
- The sample of `hooks.test` is made up (`test: true`, a session titled `Sample session`), shaped like the real payload of the hook's event; the hook's own `harness`, `project` and `workspace` filters are used for it.

### Examples

Play a sound when Claude Code finishes while you are in another app:

```toml
[[hooks]]
on = "turn.finished"
harness = "claude"
run = ["afplay", "/System/Library/Sounds/Glass.aiff"]
when_focused = false
```

**Notify your phone with [ntfy](https://docs.ntfy.sh/publish/)** when an agent asks for a permission. This is the stopgap for phone notifications until the remote work lands (`docs/spikes/2026-10-26-remote/README.md`, NOTIFY-NTFY). Install the ntfy app, subscribe to a topic nobody can guess (*"the topic is essentially a password"*: a 128-bit random string, or use access tokens) and keep the topic in a file, so the config holds no secret:

```sh
mkdir -p ~/.config/hutan && openssl rand -hex 16 > ~/.config/hutan/ntfy-topic && chmod 600 ~/.config/hutan/ntfy-topic
```

```toml
[[hooks]]
name = "Phone: an agent is waiting"
on = "approval.requested"
when_focused = false                    # at the keyboard: no buzz
shell = true
run = ['''curl -s --max-time 10 \
  -H "Title: Hutan: approval needed" -H "Priority: high" -H "Tags: lock" \
  --data-raw "$HUTAN_EVENT_SESSION_TITLE: $HUTAN_EVENT_DETAIL" \
  "https://ntfy.sh/$(cat ~/.config/hutan/ntfy-topic)"''']

[[hooks]]
name = "Phone: a turn is done"
on = "turn.finished"
when_focused = false
shell = true
run = ['''curl -s --max-time 10 -H "Title: Hutan: turn finished" -H "Tags: white_check_mark" \
  --data-raw "$HUTAN_EVENT_SESSION_TITLE ($HUTAN_EVENT_CHANGED_FILES files)" \
  "https://ntfy.sh/$(cat ~/.config/hutan/ntfy-topic)"''']
```

Rules the recipe follows, and a hook of your own should too. Send only the kind of event and the session title (the approval's title is already redacted and at most 240 characters): never a path, a diff or text you did not read, because a third-party server and Apple's push service see it. Use `--data-raw`: curl's `-d` reads a file when the text starts with `@`. Give curl a timeout. A self-hosted server works the same (`https://ntfy.example.com/…`; it must forward poll requests to ntfy.sh for instant delivery on iOS, [ntfy's config](https://docs.ntfy.sh/config/)); with access control add `-H "Authorization: Bearer $(cat ~/.config/hutan/ntfy-token)"`. Pushover is the same hook with its `https://api.pushover.net/1/messages.json` form fields (`priority=2` repeats until acknowledged). `hutan hooks test "Phone: an agent is waiting"` sends one with a sample payload. `crates/daemon/tests/it/hooks.rs` runs this command (with a loopback address and `--noproxy '*'` in place of ntfy.sh) against a server of its own, so that the recipe stays true.

Append events to a journal (the payload is on standard input; one JSON line per event):

```toml
[[hooks]]
name = "Journal"
on = "session.exited"
shell = true
run = ['cat >> ~/hutan-events.jsonl']

[[hooks]]
on = "task.done"
shell = true
run = ['echo "$(date -u +%FT%TZ) done: $HUTAN_EVENT_TASK_TITLE" >> ~/hutan-journal.txt']
```

A command can call Hutan back as you (it runs outside any session, so nothing gates it): `run = ["sh", "-c", "\"$HUTAN_BIN\" ls --json > ~/hutan-sessions.json"]`.

### What a hook can and cannot do to you

A hook is a command **you** wrote, run by the daemon with your rights whenever the event happens: as dangerous as the command. Hutan does not sandbox it. It limits what the command is told (the payload above), how many run, for how long, and who may configure them (you, by editing the file). An agent cannot list the hooks or read the log through Hutan, so a token in a hook's arguments is not exposed to it that way; an agent that can run shell commands as you can read `daemon.toml` itself, the limit `docs/control.md` § Who is asking describes. The log masks secrets in what a command printed, but a command that prints a token it was given is still the command's doing.

## Safety

Nothing here runs with more rights than the agent has. The hook script is a few lines of `sh` that checks `HUTAN_ENV`, finds `HUTAN_BIN`, and calls `hutan agent hook`; the daemon validates every resume command before it stores it. No credentials pass through any of it.

Tests: user hooks `crates/daemon/src/hooks.rs`, `crates/daemon/tests/it/hooks.rs`, `crates/cli/tests/it/hooks.rs`, `crates/desktop/tests/it/hooks_gpui.rs`; `crates/daemon/tests/it/agent.rs` (protocol semantics, restart resumes a fake agent through its reported argv), `crates/cli/tests/it/{agent,integration,integration_mcp,omp_extension}.rs` (CLI against a real daemon; installs on temporary copies of real-shaped configuration; the OMP extension under Bun, skipped when Bun is missing).

Tests: `crates/daemon/tests/it/agent.rs` (protocol semantics, restart resumes a fake agent through its reported argv), `crates/cli/tests/it/{agent,integration,integration_mcp,omp_extension}.rs` (CLI against a real daemon; installs on temporary copies of real-shaped configuration; the OMP extension under Bun, skipped when Bun is missing).

## Several accounts and the agent's own terminal

`hutan account add <harness> <name>` (ADR 0015 § Amendment, `docs/agents.md` § Several accounts per harness) gives a harness a second login in a folder of Hutan's (`<data dir>/accounts/<harness>/<name>/`). Chats Hutan starts run with it. A harness you start yourself in a terminal does not: to use an account there, set the harness's variable to the folder `hutan account list --json` shows as `home` (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR` with `OMP_PROFILE=` empty). The hooks written by `hutan integration install` go to the harness's own configuration folder, not into an account's: an account starts as a first run of the harness.
