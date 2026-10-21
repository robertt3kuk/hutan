# Agent control surface: UI commands, origin, policy, `hutan mcp`

ADR 0016, slice M8-C, protocol 0.21, capability `control`; Mawas's safeguards: ADR 0017, slice MW-1, protocol 0.31, capability `control.grants` (§ Mawas). Anything in Hutan an agent (or Mawas, the assistant) could ask for is a daemon request; there are three doors to the same set — the CLI (`hutan …`), the MCP server (`hutan mcp`) and the desktop, which also *executes* the UI requests the daemon routes to it. The daemon decides, per request, whether the caller is the user or an agent, and what an agent may change.

Wire details: `docs/protocol.md` § Control. Code: `crates/protocol/src/ui.rs` (DTOs), `crates/daemon/src/{control,uihub,mcp_wiring}.rs` and the classification table in `router.rs`, `crates/cli/src/{commands/ui.rs,mcp/,integration/mcp.rs}`, `crates/desktop/src/workbench/remote.rs`.

## Driving the window

The desktop publishes (`ui.publish`) what it can do and what it shows, on connect, after every reconnect and whenever it changes (debounced to 250 ms):

- **Actions**: every palette command by its GPUI action name (`workbench::SplitRight`, `workbench::ToggleLeftDock`, …) with its title. Variants of one action (`MoveActivePanel`, `SetWorkspaceSort`, …) are one entry whose `args_schema` takes `choice`, the title of the variant. Parameterised actions: `workbench::ApplyPreset {name}`, `workbench::OpenPanel {id}`, `workbench::OpenSession {id}` (opens or focuses the session's tab), `workbench::OpenWorkspace {id}`, `workbench::OpenFile {path, line?, column?}` (E-5: opens or focuses the file's tab and shows the line; `path` absolute; the file is read through the daemon without `picked`, so only below the workspace roots; a refused or missing file ends in a notice after the dispatch, as `hutan open` does).
- **Settings**: every key of the settings schema with `namespace`, `key`, `type` (`bool`, `enum`, `integer`, `number`, `string`, `string_list`, `unsupported`), `doc`, `values` of an enum, the effective `current` value, and the `layers` it can be written to. `user` is the user's `settings.toml`. `workspace` and `project` (offered only for namespaces a workspace may override) write a `.hutan/settings.toml`, creating the file and directory, and are schema-validated like `user`: `project` is the folder of the project of what the window focuses (the project of the session in the focused pane, `WorkspaceInfo.project.root`, else the shown workspace when it is a project); `workspace` is the file the settings store currently layers over the user's (the folder of the shown workspace). A `project` write for a folder that is not the one in effect is only written, and applies when that project is shown. Without a focused project (or, for `workspace`, a workspace folder) the write is `invalid_state`; for a namespace that is not overridable it is `bad_request`. `app.keep_running` is `unsupported` here: it registers a launch agent, use the action `daemon::ToggleKeepRunning`.
- **Layout summary**: `window_id`, `workspace_id`, `preset`, `docks` (position, visible, panels, active), `panes` (the split tree; each pane lists its items — `terminal`, `chat`, `panel`, `placeholder`, `changes` (a checkout's Changes view), `file` (a file tab, titled with the file name; protocol 0.35 adds its canonical absolute `path`, what `ui.run OpenFile` takes) — with the session id or panel id and title, and the active index), `focused`, `zoomed`, `appearance`, whether the window is active.

`ui.run {action, args?, window_id?}` goes to the window (default: the one that was active most recently) as the event `ui.command`; the window runs it **through the same action path as the keyboard and the palette** (it dispatches the action at the focused element), and answers with `ui.command_result`. The caller gets that answer, or `internal` “the window did not answer” after 5 s. `ui.set_setting {namespace, key, value, layer}` is executed the same way: the desktop checks the key and the value against the settings schema (a bool must be true/false, an enum one of its values, …), writes the TOML layer and hot reload applies it.

| Situation | Error |
|---|---|
| no window is connected | `invalid_state` (“no window is open”) |
| unknown action, setting, preset, panel, session or workspace | `not_found` |
| missing or wrongly typed arguments, value the schema refuses, layer that cannot be written | `bad_request` |
| the action has no handler where the window's focus is now | `invalid_state` |
| the window did not answer in time | `internal` |

The answer of `ui.run` is `{window_id, result: {ran: <action>}}`. Some actions finish after the dispatch (a split starts a terminal first): read the outcome with `ui.layout`, which follows the change within the debounce. A **setting** (`ui.set_setting`) is answered after the window has applied it and published its catalog again, so what it changes about the actions (`assistant.enabled` lists or removes `workbench::OpenAssistant`) is already in `ui.actions` when the answer arrives.

An action that closes the window (`workbench::Quit`, or the quit of a closed last window) ends the window's connection before it can answer, and the daemon answers its caller `invalid_state` “the window closed before it answered”. The CLI reports that as success: `hutan ui run workbench::Quit --json` prints `{"window_id": …, "result": {"ran": "workbench::Quit", "window_closed": true}}`, human output `ran workbench::Quit; the window closed`, exit code 0. (Quit with live sessions first asks which to stop; that card answers `ran` and the window stays.) Every action `ui.actions` lists is dispatchable: the desktop's `every_action_the_window_lists_dispatches` test runs each one.

CLI: `hutan ui actions [--filter TEXT] [--json]`, `hutan ui layout [--json]`, `hutan ui run <action> [--arg k=v]…`, `hutan ui set <namespace.key> <value> [--layer user|workspace|project]`; `--window ID` picks a window.

## Who is asking

`ClientInfo.origin {session_id}` is sent by the CLI and by `hutan mcp` from `HUTAN_SESSION_ID`, which every Hutan session process gets. The daemon honours it only while that session exists and is live (a claim that does not hold is ignored and logged: the caller is then the user).

A connection is **agent-originated** only when its origin session is

- a chat session (`harness` or `direct`: Codex, ACP harnesses, the assistant), or
- a terminal session in which an agent reported itself (`SessionInfo.agent` is present, see `docs/integrations.md`; it is gone again after the agent releases itself).

**A plain shell session without a reported agent is the user**: `hutan stop` typed in a Hutan terminal is never gated. The check is made for every request, not once at `hello`, so a long-lived `hutan mcp` follows an agent that reports itself later or leaves.

MCP servers Hutan starts for chat sessions (below) get `HUTAN_SESSION_ID` of that very session in their environment, explicitly.

**Limits.**
- An agent that runs in a terminal without a Hutan integration (no hooks, so no report) is not detected: its commands count as the user's. Install the hooks (`hutan integration install <agent>`) to get it gated.
- The origin is an environment variable: an agent can unset `HUTAN_SESSION_ID` or edit its hello. It is a marker for well-behaved agents and for the MCP servers Hutan itself starts, not a security boundary against a hostile one; that boundary is the user's sandbox for the agent.
- An `hutan mcp` started by the user's own agent configuration outside a Hutan session has no origin and is the user's (nothing is gated).

## The policy

`daemon.toml`:

```toml
[control]
agents = "ask"                # "ask" (default), "allow" or "read_only"
mcp = true                    # chat sessions Hutan starts get `hutan mcp`
approval_timeout_secs = 600   # an unanswered approval is refused after this long
```

Every request kind is classified once, in one table in `crates/daemon/src/router.rs` (an exhaustive `match`: a new request that is not classified does not compile; a test checks the table against the committed fixtures):

- **Read** — always allowed: lists, snapshots, history, `ui.actions`, `ui.layout`, `approval.list`, `account.list`, terminal attach/detach/ack, `agent.report`/`release` (what hooks do), `client.focus`, the file service's reading side — `fs.read|stat|list|find|search|search.cancel|watch|unwatch` and `fs.write.abort` (protocol 0.33) — …
- **Mutate** — follows `agents`: creating, stopping, renaming, sending, tasks, worktrees, workspaces, history index changes, `ui.run`, `ui.set_setting`, `session.send_text`, `daemon.shutdown`, … `workspace.assign` and `workspace.delete` (projects, protocol 0.19) are Mutate; `workspace.list|recent` Read. The resource manager (`docs/resources.md`): `session.kill`, `process.signal` and `agent.interrupt` are Mutate; `resources.list|subscribe|unsubscribe` are Read. `fs.write.begin|chunk|commit` (protocol 0.33) are Mutate: see § Files.
- **UserOnly** — refused to agents under every policy: `approval.respond`, `notify.act` (it can answer an approval), provider configuration and credentials (`provider.add|update|remove|set_credential|clear_credential`, `account.login|logout`), the desktop's own `ui.publish` and `ui.command_result`, the daemon's own settings: `daemon.settings.set` and `assistant.reset` (protocol 0.28), and Mawas's grants: `control.grants.list` and `control.grants.revoke` (protocol 0.31), and the user's side of Mawas's memory: `memory.write|update|archive|restore`, `memory.proposals.list|resolve` (protocol 0.32, § Memory). `[control]` itself lives in `daemon.toml`, which only `daemon.settings.set` writes, so an agent can never widen its own permissions, whatever `agents` says; `daemon.settings.get` is Read. Neither is a tool of `hutan mcp`.
- `notify.mute` (protocol 0.34) is **UserOnly** too: what the user is told about is the user's, and an agent must not silence its own approvals.
- The chat queue (protocol 0.36): `chat.queue.add|edit|remove|clear` and `chat.send_now` are **Mutate**, like `chat.send` (they send a message, later or now; `chat.send_now` may interrupt the turn); `chat.queue.list` is **Read**.

For a Mutate request from an agent:

| `agents` | What happens |
|---|---|
| `ask` | an approval of kind `control` is raised and the request waits |
| `allow` | it runs |
| `read_only` | refused with `forbidden` (“agents are read-only here …”) |

The `control` approval is the existing approval machinery: it shows in `hutan approvals`, the desktop's approval card and the native notification (`approval_required`), and is answered with `hutan approve <id> <option>` or the card. The title says in plain words what is asked and on which session (“Agent in session “api” asks Hutan to rename session “build” (…) to “x””), the detail carries the request's parameters (cut at 400 characters). Options: `allow_once`, `allow_session` (remembered for this origin session and this kind of request — per action for `ui.run`, per namespace for `ui.set_setting` —, until the daemon restarts), `deny`. Unanswered after `approval_timeout_secs` the request is refused (`forbidden`, outcome `expired`). A request that waited says so in its result (`control: {decision, waited_ms}`; the MCP text says “The user approved this request after waiting …”); a denial says not to retry.

Terminal input frames carry no response and cannot wait for an approval: an agent-originated connection may send them under `allow`, never under `read_only`, and under `ask` only after the user chose “allow for this session” for `session.send_text` (agents should use `hutan send` / `session_send_text`, which ask).

**Audit.** Every decision about an agent's mutation is a row of kind `control` in the `approvals` table of the daemon's database, with the title, the origin session, the option (`allow_once`, `allow_session`, `deny`, or for decisions nobody was asked about `allow_policy`, `allow_grant`, `deny_policy`, `deny_user_only`), the client that answered (`desktop`, `cli`, … or `policy`) and the time.

## Mawas

ADR 0017 decision 4, slice MW-1, protocol 0.31 (capability `control.grants`). **Mawas** is the assistant (`docs/assistant.md`): it makes tool calls — its harness's own and Hutan's, through the `hutan mcp` server every Hutan-started chat gets — and the daemon, not only the harness's flags, decides what they may change. A request whose origin is a Mawas chat (`ChatInfo.assistant`, or a provider assistant: `agent: "assistant"`) is gated by **`assistant.mode`**, whatever `[control] agents` says:

| Mode | A mutating request (the **Mutate** class above) |
|---|---|
| **Ask before acting** (default) | raises a `control` approval, even under `agents = "allow"`; a stored grant may answer it instead; `agents = "read_only"` still refuses it |
| **Advisor** | `forbidden`, `error.data.control.decision = "advisor"`: "Mawas is in Advisor mode: it can read and advise, not change anything, so `session.rename` was refused" |

Reads always run. **UserOnly** requests stay refused (`user_only`) — answering approvals and questions, `daemon.settings.set`, `assistant.reset`, provider credentials, account logins, and `control.grants.list|revoke` — so Mawas can never widen its own permissions, read or revoke its grants, or answer an approval, its own included. The mode itself is `assistant.mode` of `daemon.toml`, a user-only setting.

**The approval.** Its title says what is asked ("Mawas asks Hutan to rename session “build” (…) to “release”"), its detail carries the request's parameters, and its options are **Allow once** (`allow_once`), **Allow for this chat** (`allow_chat`), **Always allow this tool in this workspace** (`allow_workspace`) and **Deny** (`deny`). It shows in the chat, in the *Today* rail of Mawas Home, in `hutan approvals` and as a native notification, and is answered from any of them (`hutan approve <id> allow_chat`).

**Grants.** *Allow for this chat* and *Always allow this tool in this workspace* store a grant — `control_grants(id, tool, workspace_id NULL, chat_session_id NULL, created_ms, created_by)`, schema 12 — which the daemon consults before it asks. `tool` is the grant key of the request: the method, narrowed for `ui.run` (per action) and `ui.set_setting` (per namespace). A chat grant covers that chat only; a workspace grant every Mawas chat of that workspace, and never another workspace. Grants survive a daemon restart. `hutan grants ls [--ws ID]` and `hutan grants revoke <id>|--all` (and Settings → Mawas) list and remove them; both are user-only requests (`control.grants.list`, `control.grants.revoke`).

**What always asks.** A grant never covers a destructive or irreversible request: `session.kill`, `session.stop`, `session.close` with `stop_running`, `process.signal`, `workspace.delete|close|remove_root`, `worktree.remove|merge`, `task.delete`, `session.send_text` (anything a shell can do), `daemon.shutdown`, `history.clear`, `assistant.configure`, `ui.run workbench::Quit` and `ui.set_setting` of the `assistant` namespace. Their approval offers only *Allow once* and *Deny* (the detail says "This kind of change always asks: no grant covers it"); a grant row the database holds for one of them is ignored. Choosing an option that was not offered is `option_not_offered`.

**The turn limit.** At most `assistant.max_actions_per_turn` (default **10**, 1–1000, a live daemon setting) changes per turn run without asking: the ones a grant answered and the ones the user allowed once count. The next one always asks, whatever is granted, with the title "Mawas has made 10 changes this turn. Mawas asks Hutan to …" and the options *Allow once* / *Deny*; the count starts again with the next turn.

**Audit.** Every decision about a Mawas request is a row of kind `control` in the `approvals` table (`option_id`: `allow_once`, `allow_chat`, `allow_workspace`, `deny`, `allow_grant`, `deny_advisor`, `deny_policy`, `deny_user_only`, or an expiry; `decided_by`: `desktop`, `cli`, … or `grant:<id>` / `policy`) and a line of the daemon log with target `hutan::audit` (`actor=mawas session=… workspace=… action=session.rename decision=allow_grant by=grant grant_id=7`); creating a grant is part of its approval's row, revoking one is a line of its own. In the chat the tool call is the harness's own tool card, and its result begins with the outcome the `hutan mcp` server adds: `[The user approved this request after waiting 4.2 s, and allowed this tool for this chat.]`, `[Allowed by a grant the user gave earlier …]`, `DENIED by the user: …`, `REFUSED: Mawas is in Advisor mode …`.

**Harness launch per mode.** The daemon starts Mawas's harness the way the mode says (`crates/daemon/src/harness.rs`, `assistant_profile`), and every permission request of the harness itself reaches the same approval cards (`session/request_permission` of ACP, Codex approvals):

| | Advisor | Ask before acting |
|---|---|---|
| OMP | `omp acp --tools read,grep,glob --append-system-prompt <read-only prompt>` | all tools, `--approval-mode always-ask` (everything above reading asks the client, even when the user's own OMP says `yolo`), `--append-system-prompt <Mawas prompt>` |
| Claude Code | session mode `plan` | session mode `default` (its own default permission mode) |
| Codex | `sandbox: read-only`, `approval_policy: never` | `sandbox: workspace-write`, `approval_policy: on-request` |
| Provider (direct chat) | no tools | no tools: *Chat only* until the gateway has a tool loop |

A change of `assistant.mode` applies from the **next turn**: when a Mawas message arrives, an agent that runs in the other mode is stopped and started again in the current one (the stored conversation continues: `session/load`, `thread/resume`), the journal says so ("Mawas is now in “Advisor” mode … Its agent restarted so the change applies to this turn."), and a turn in progress or a pending approval finishes first. The daemon-side gate follows the new mode at once.

**Limits of origin attribution.** As above: the origin is an environment variable and a marker for well-behaved agents, not a security boundary. A Mawas whose harness ran a shell command could unset `HUTAN_SESSION_ID` and talk to the daemon as the user; that is why a native harness tool (a command, a file edit) asks too, and why Ask before acting runs OMP with `always-ask` rather than trusting its configuration. A stronger restricted endpoint with a sandbox comes with unattended schedules (ADR 0017: MW-9, MW-10).

## Memory

Protocol 0.32, capability `memory`, slice MW-2 (`docs/assistant.md` § Memory). Mawas's memory is the user's: an agent-originated connection — Mawas, or any agent session — may **read** it (`memory.list`, `memory.history`; MCP `memory_list`, `memory_search`) and **propose** (`memory.propose`; MCP `memory_propose`), nothing more. `memory.write`, `.update`, `.archive`, `.restore`, `memory.proposals.list` and `.resolve` are **UserOnly**: `forbidden` with `error.data.control.decision = "user_only"`, whatever `[control] agents` says, and `approval.respond` is user-only already, so an agent can neither apply its own proposal nor read the queue of others'. `memory.propose` is classified Read: it changes nothing, so it does not wait for an approval of the control gate (not in Advisor mode either, and not under `read_only`); instead the proposal raises its own `control` approval — options `approve` / `reject`, title `Memory proposal: …` — which only the user answers, and nothing is applied before. The daemon stamps where a proposal came from (the connection's verified origin session and its open turn) and the model cannot claim another source; a proposal needs an origin (`bad_request` without one). A body that looks like a credential is refused for the proposal as for the user's write. Memory text is data: it cannot grant tools, widen a scope or override a policy, and the block Hutan hands a conversation says so.

## Files

Protocol 0.33, capabilities `fs` and `fs.watch`, slice E-3 (`docs/protocol.md` § Files). An agent-originated connection — a chat session, or a shell where an agent reported itself, or `hutan fs` typed there — reads files through the daemon as freely as the user: `fs.read`, `fs.stat`, `fs.list`, `fs.find`, `fs.search` and `fs.watch` are **Read**, confined to the roots the daemon tracks. Two things differ from the user's:

- **`picked` counts for nothing.** A path outside the workspace roots is `forbidden` (`outside_roots`) for an agent even with `picked: true`; only the user choosing a file in a dialog or naming it on their own command line may reach outside the roots.
- **Writing asks.** `fs.write.begin` is **Mutate**: under `agents = "ask"` an approval of kind `control` is raised ("Agent in session “api” asks Hutan to overwrite the file /repo/src/main.rs", detail: the request's parameters), under `read_only` the write is `forbidden`, under `allow` it runs and is recorded. The approval covers the whole write: the `fs.write.chunk`s and `fs.write.commit` that follow on the same connection, for an upload the daemon holds for it, are not asked again (`FileService::continues_upload`; they are classified Mutate in the table all the same, so an upload id that is not the connection's own is gated like any mutation). *Allow for this session* remembers the method (`fs.write.begin`), not a path: every later file write of that session runs unasked; Mawas's grants work the same per tool. Every commit is audited (`fs.write`, the canonical path, in the daemon log with target `hutan::audit`).

A write by an agent through the service is attributed in `fs.changed` events of other clients as `origin: {kind: "agent", confidence: "session"}`; edits an agent makes with its own tools reach the same events through the `file_diff` heuristic (`docs/protocol.md` § Files, Origins).

## `hutan mcp`

A stdio MCP server (hand-rolled JSON-RPC, one message per line; protocol version 2025-06-18, also 2025-03-26 and 2024-11-05). The agent's host starts it; it connects to the daemon like any client (`--socket`, `HUTAN_SOCKET`, `HUTAN_HOME`) and sends `HUTAN_SESSION_ID` as its origin. Tools are declared once in `crates/cli/src/mcp/table.rs`: a name, a description written for a model, a JSON Schema and the daemon request it maps to; unit tests check that every row decodes into a real request, that every schema is valid, and that the `readOnlyHint` agrees with the classification above. Failures of a tool are results with `isError` (the model sees them); `tools/call` runs in the background so a call waiting for an approval does not block the loop, and a cancellation aborts it.

| Tool | Request | Mutates | What it does |
|---|---|---|---|
| `session_list` | `session.list` | no | sessions: id, title, state, directory, workspace, the agent reported in it |
| `session_create` | `session.create` | yes | start a terminal session (command, directory, workspace, title) |
| `session_send_text` | `session.send_text` | yes | type into a terminal session, optionally pressing enter |
| `session_output` | `session.list` + terminal snapshot | no | the last lines of a terminal session |
| `session_stop` | `session.stop` | yes | stop a session |
| `session_restart` | `session.restart` | yes | start an ended session again |
| `session_kill` | `session.kill` | yes | SIGKILL a session's whole process tree |
| `session_close` | `session.close` | yes | stop a session and remove it from the list (a busy one needs `stop_running`) |
| `session_reopen` | `session.reopen` | yes | bring a closed session back |
| `process_signal` | `process.signal` | yes | signal one process of a session (a runaway build) |
| `agent_interrupt` | `agent.interrupt` | yes | stop an agent's turn (or one subagent) without ending the session |
| `resources_list` | `resources.list` | no | process trees with CPU and memory per session |
| `session_rename` | `session.rename` | yes | change a session's title |
| `chat_create` | `chat.create` | yes | start a chat with an agent (Codex) or a model directly |
| `chat_send` | `chat.send` | yes | send a message to a chat |
| `chat_log` | `chat.history` | no | the journal of a chat |
| `approval_list` | `approval.list` | no | what waits for the user (agents cannot answer: there is no respond tool; Mawas's grants are not a tool either) |
| `task_add` | `task.create` | yes | add a task with dependencies |
| `task_list` | `task.list` | no | tasks and their state |
| `task_start` | `task.start` | yes | start a ready task |
| `task_done` / `task_cancel` / `task_reset` | `task.update` | yes | mark done, give up, run again |
| `task_review` | `task.review` | yes | accept a done task's work, or ask for changes with a note sent to its chat (0.25) |
| `worktree_new` | `worktree.create` | yes | a git worktree for a branch |
| `worktree_list` | `worktree.list` | no | worktrees and the sessions in them |
| `worktree_diff` | `worktree.diff` | no | the files a worktree changed against its base (commits plus uncommitted) |
| `worktree_file_diff` | `worktree.file_diff` | no | one changed file's unified diff |
| `worktree_merge` | `worktree.merge` | yes | merge a worktree's branch into the main checkout's branch (local commit; refusals and conflicts change nothing) |
| `worktree_commit` | `worktree.commit` | yes | stage everything in a worktree and commit it, so a merge takes it (0.25) |
| `worktree_rm` | `worktree.remove` | yes | remove a worktree |
| `checkpoint_list` | `checkpoint.list` | no | checkpoints of agent turns, newest first: what the user can rewind to; the agent cannot restore (0.37) |
| `checkpoint_diff` | `checkpoint.diff` | no | what a turn changed, or what changed since it started (`against: now`); bounded like `worktree_diff` (0.37) |
| `workspace_list` | `workspace.list` | no | workspaces and projects |
| `project_recent` | `workspace.recent` | no | recently opened folders |
| `workspace_create` | `workspace.create` | yes | a workspace (group) with folders and projects |
| `workspace_assign` | `workspace.assign` | yes | put a project into a workspace, or out of it |
| `workspace_delete` | `workspace.delete` | yes | delete a workspace (its projects stay) |
| `workspace_open` | `workspace.open` | yes | open a folder as a workspace (shown with `ui_run workbench::OpenWorkspace`) |
| `notification_list` | `notify.list` | no | pending notifications |
| `history_search` | `history.search` | no | search past sessions of Hutan, Claude Code, Codex, OMP |
| `ui_actions` | `ui.actions` | no | what the window can do, with argument schemas, and its settings |
| `ui_layout` | `ui.layout` | no | what the window shows |
| `ui_run` | `ui.run` | yes | run an action of the window |
| `open_file` | `ui.run` (`workbench::OpenFile`) | yes | show the user a file at `line` / `column` (E-10): a file tab, or the one that has it; `path` absolute and below the workspace roots |
| `ui_set_setting` | `ui.set_setting` | yes | change a setting |

`hutan help agent` prints the same guide for agents that only have a shell (what is allowed, that changes may wait for approval, exit codes).

**Checkpoints** (protocol 0.37): `checkpoint.list` and `checkpoint.diff` are reads, free under every policy. `checkpoint.remove` is a change: under `ask` it raises a `control` approval ("delete the checkpoint …"), `read_only` refuses it, `allow` runs it; for Mawas it always asks (it destroys what a restore needs). There is no MCP tool to remove or restore; restoring will be user-only (CK-4).

## Wiring

- **Hutan-started agents.** With `[control] mcp = true` the chat manager hands every agent it starts the `hutan mcp` server: `ChatManager::mcp_server(session)` returns the spec (command = the `hutan` executable sessions get as `HUTAN_BIN`, args `mcp`, env `HUTAN_SESSION_ID` of that session and `HUTAN_SOCKET`). Codex chats get it as `-c mcp_servers.hutan.command=…`, `.args=…`, `.env=…` overrides of `codex app-server`. The ACP harnesses (`omp`, `claude`) get it in `mcpServers` of `session/new` / `session/load` (`McpServerSpec::acp_server`, next to the user's `[mcp.<name>]` servers).
- **The user's own agents.** `hutan integration install|uninstall|status <codex|claude|omp> --mcp` adds, removes or checks a marked `hutan` server in the agent's own configuration: `[mcp_servers.hutan]` in Codex's `config.toml`, `mcpServers.hutan` in Claude Code's `.claude.json` and OMP's `mcp.json`. Only that explicit command writes; the entry is marked by `env.HUTAN_MCP_ENTRY = "hutan-managed"`, an entry that is not marked is never changed (the install fails and says so), the file is backed up first (`<name>.hutan-<unix ms>.bak`), a second install changes nothing. Without `--mcp` the command manages the hooks as before and writes no server entry. An agent started this way runs outside a Hutan session unless the user starts it in one, so see *Who is asking* for what is gated.

## Tests

`crates/daemon/tests/it/mawas.rs` (Mawas's gate: an approval even under `agents = "allow"`, Allow for this chat and Always allow here with their limits, a destructive request that asks despite a grant, an agent that cannot list, revoke or answer, Advisor's refusal, the turn limit, the audit log and table, grants across a restart and their revocation, the mode change restarting the agent), `crates/cli/tests/it/grants.rs` (`hutan grants ls|revoke`, refused inside an agent's session), `crates/daemon/tests/it/control.rs` (routing with a fake desktop, timeout, no window, which sessions count as agents, policy modes, approvals, `allow_session` memory, audit), `crates/daemon/src/{control,router}.rs` unit tests (classification table, descriptions, grants), `crates/desktop/tests/it/control_gpui.rs` (the window publishes its catalog and layout, publishes again after a reconnect, executes split / toggle / preset / panel / focus session / setting and answers ok or error), `crates/cli/tests/it/{control,mcp,integration_mcp}.rs` (`hutan ui …` against a fake desktop, the approval flow of an agent-reported session next to a plain shell's ungated one, MCP over stdio against a test daemon, `--mcp` on temporary agent configurations). `crates/daemon/tests/it/files.rs` (the file service: confinement and `picked`, chunked round trips, version conflicts, upload bounds, watching with origins, and the approval an agent's `fs.write` waits for: once for the whole write, denied, `read_only`, `allow`), `crates/cli/tests/it/files.rs` (`hutan fs` and `hutan open`, the conflict path).
