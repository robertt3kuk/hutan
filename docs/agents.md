# Agent chat sessions (Codex, OMP, Claude Code)

Decisions: ADR 0005 (agent integration strategy), ADR 0006 (SQLite), ADR 0008 (credentials), ADR 0015 (harness chat over ACP, accounts). Codex first; § ACP harnesses covers OMP and Claude Code. Wire messages: `docs/protocol.md` § Agent chat and approvals. Roadmap: M4.1–M4.3, M4.5 (the chat view is M4.4).

A chat session is a conversation with a coding agent that Hutan's daemon runs: one `codex app-server` process per session, spoken to over stdio JSON-RPC, mapped to one internal event model, journaled in Hutan's own database, and shown by any client (today the CLI). The agent's login is Codex's own (`codex login`); Hutan never reads or stores its credentials (ADR 0008).

## Parts

| Where | What |
|---|---|
| `crates/core/src/agent.rs` | `AgentEvent` (+ `AgentEventDraft`, `ApprovalPolicy`, `SandboxMode`, `ModelInfo`): the unified event model |
| `crates/agents` | `AgentAdapter` trait, `CodexAppServerAdapter`, the JSON-RPC transport, and the fixture tool (`hutan-codex-fixture`) |
| `crates/daemon/src/chat.rs` | `ChatManager`: adapters, one pump task per session (numbers, journals and publishes events), the approval engine |
| `crates/daemon/src/registry.rs` | the session records: create / stop / restart / restore of chat sessions |
| `crates/storage` (schema v3) | `chat_items`, `chat_turns`, `approvals` |
| `crates/client`, `crates/cli` | `Client::chat_*`, `Client::approvals/approve`, the store's pending approvals; `hutan chat …`, `approvals`, `approve` |

## The adapter

`CodexAppServerAdapter::start(config, spec)` spawns `codex app-server`, then:

1. `initialize` (client name `hutan`, experimental API off) and the **version check**: the server's `userAgent` carries its version; only `>=0.160, <0.170` is accepted (`SUPPORTED_VERSIONS`). Anything else — or a version it cannot read — fails with a message naming the version found and the supported range. The wire structs in `crates/agents/src/codex/wire.rs` follow the schema of 0.162.1; they list only the fields Hutan reads and tolerate everything else.
2. `model/list` and **model choice**: the requested model must be in the catalog (the error lists what is); without one the catalog's default is used. The model named in the user's `~/.codex/config.toml` is never trusted: an older CLI can reject it after a successful handshake (`docs/spikes/2026-10-13-agent-protocols.md`).
3. `thread/start` with `model`, `cwd`, `approvalPolicy`, `sandbox` and `approvalsReviewer: "user"` (approvals always come to Hutan), or — when the session has a stored provider thread id — `thread/resume` with `excludeTurns: true`: the provider's copy of the history is not needed, Hutan's journal is the history. Both also carry `developerInstructions` (a typed field of Hutan's DTOs, `StartSpec.developer_instructions`) when the session is a Mawas assistant with memory to give (`docs/assistant.md` § Memory); `baseInstructions` is never sent, so Codex keeps its own.

Then a pump task turns every message of the process into events. `send_message` is `turn/start` (it answers early; the turn's progress arrives as notifications), `interrupt` is `turn/interrupt`, `respond` answers a server request.

### What becomes what

| Codex | `AgentEvent` |
|---|---|
| `turn/started`, `turn/completed` (`completed`/`interrupted`/`failed`) | `turn_started`; `turn_completed` / `turn_cancelled` / `turn_failed` (a JSON backend error is reduced to its `error.message`, the raw text kept in `details`) |
| `item/started`/`completed` `userMessage`, `agentMessage` (+ `item/agentMessage/delta`) | `message_start` / `message_delta` / `message_end` (role, phase, the complete text) |
| `reasoning` (+ summary/text deltas) | `reasoning_start` / `reasoning_delta{summary,content}` / `reasoning_end` |
| `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `webSearch` | `tool_start` (name, one-line title, input) … `tool_update` … `tool_end` (status, output cut at 256 KiB with `truncated`, exit code, duration) |
| `item/commandExecution/outputDelta` | `command_output` (live only) |
| `fileChange` changes, `item/fileChange/patchUpdated`, `turn/diff/updated` | `file_diff` (per file `patch`, or the turn's aggregate `patch`) |
| approval requests of the server | `permission_requested`; `serverRequest/resolved` or the turn's end → `permission_resolved` |
| `turn/plan/updated`, `plan` item | `plan` (steps, or prose) |
| `thread/tokenUsage/updated` | `usage` (fields the server did not send stay absent) |
| `error`, `warning` | `notice` |
| anything else | `opaque { method }` with the raw `payload` (account, rate-limit and startup chatter is dropped) |

Messages of other threads (sub-agents) are ignored. A notification that does not fit its schema also becomes `opaque`: the adapter never fails on what it does not understand. Server requests Hutan cannot serve (`item/tool/requestUserInput`, MCP elicitation, token refresh, …) are answered with JSON-RPC "method not found" and noted as a `notice`, so the agent never waits for them.

## Approvals

Codex asks before running a command, applying a patch, or widening its permissions (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`). The adapter turns each into a `permission_requested` event whose `options` are exactly the decisions the server listed in `availableDecisions` (when it lists none, the schema's `accept`, `acceptForSession`, `decline`, `cancel`); permission profiles offer `grant`, `grant_session`, `deny`. Each option keeps the provider's own decision (`options[].provider`) and gets a `kind` for styling.

The daemon owns the rest (`ChatManager`):

- A request becomes pending in daemon state (`approval.list`, `approval.required`). It survives any client disconnect; the session shows `waiting_approval`.
- `approval.respond` is accepted only with an `option_id` the request offered (`option_not_offered` otherwise, and nothing reaches the agent); the adapter checks again. A request is answered once.
- A request **expires** when its turn ends (completed, failed, interrupted), when Codex withdraws it, when the agent process exits, and when the daemon restarts. `approval.resolved` says `selected` (with the option) or `expired`.
- Audit: every request is a row of `approvals` (kind, title, offered options, time); its end adds the time, outcome, chosen option and which kind of client answered.
- Provenance (protocol 0.15, capability `chat.provenance`): the same fact travels with the answer: `approval.resolved` and the journaled `permission_resolved` event carry `decided_by: {client_kind, client_name?}` (`client_kind` is the answering connection's `hello` kind: `desktop`, `cli`, `notifier`, `other`). The first answer the agent accepts is credited; a refused attempt (an option that was not offered, an approval already answered) is not. An expiry has no `decided_by`. The adapter does not know who answered; the daemon (`ChatManager::attribute`) fills it in before the event is journaled, so `chat.history` replays it. `hutan chat log` prints `approval <id>: accept (decided by cli)`.

Codex's own approval policy decides whether a command asks at all (`untrusted`: everything but known-safe commands; `on-request`: the agent decides; `never`); a safe command like `echo` may run without asking even under `untrusted`.

## Journal and restore

The pump gives every event the session's next `seq` and a timestamp. Everything but the streaming deltas is written to `chat_items` (`event_json` = the whole event), turns to `chat_turns`; `chat.history` reads it back (marked `replay`). Streaming events are delivered live and not stored — the `*_end` events carry the full text — so `seq` has gaps.

**Snapshots of streaming text (0.15).** So that a window opened mid-stream shows the text so far, `Live` keeps a `Partials` (`crates/daemon/src/partial.rs`) beside the journal: the text of every open assistant message and reasoning block of the running turn (one entry per open item, at most 1 MiB of text each). The pump journals a `message_partial` / `reasoning_partial` event for the items that changed at most every 500 ms (a one-shot timer armed when text arrives that no snapshot has; nothing runs while nothing streams) and for what is left when a turn ends. A snapshot is **replaced in place**: writing the new row deletes the item's previous one in the same transaction, so the journal holds one row per open item however many deltas arrive, and the item's `message_end` / `reasoning_end` deletes it. Numbering an event and folding it into `Partials` happen under one lock, which is what makes a snapshot's meaning exact: it includes every delta of its item with a smaller `seq`. `chat.history` takes the lock, answers an open item with a fresh snapshot (a new `seq` from the session's counter, never journaled by that call; the stale journal row of the item is left out) and reads `last_seq` under the same lock; live deltas that follow the reply have a greater `seq`. A snapshot that is all that is left of a message (the turn failed or was interrupted before `message_end`, or the daemon died mid-stream) stays in the journal and the chat shows it as the text the message had. The journal grows with use and is deleted with its session; there is no compaction yet.

The `ChatSpec` of a chat (agent, model, policies, **provider thread id**) is the session row's `spec_json`. After a daemon restart (`restore_sessions = "respawn"`, the default) live chat sessions start again: `thread/resume` with the stored thread id, the same session id, history unchanged. `restore_sessions = "stopped"`, a stopped session and a failed one stay listed; `hutan restart <id>` (`session.restart`) resumes them. An open turn at the time of a crash is closed in the journal (`turn_failed` / `turn_cancelled`) and approvals left open become `expired`. The restart is said on the session (`SessionInfo.recovery`, `chat_resumed` with `last_turn: completed | interrupted`, or `not_restarted` with the agent's error; `docs/protocol.md` § Recovery): the desktop's chat header offers *Retry last message* for an interrupted turn, which sends the journal's last user message again (attachments are not resent). A shutdown that cancels an open turn marks its row (`hutan stopped during the turn`) and the approvals it expires (`decided_by` `daemon_stop`), so the next start can tell it from a turn the user cancelled. Provider resume only continues the agent: Codex's resumed thread omits tool items and renumbers ids, which is why the history shown comes from Hutan's journal.

## Checkpoints

Every turn of a Codex, OMP or Claude Code chat whose folder is in a git checkout gets a **checkpoint** (ADR 0019, on by default): a snapshot of the checkout's files just before the prompt reaches the agent and another when the turn ends — however it ends (completed, failed, interrupted, the agent exiting, the daemon stopping). The snapshots are commits in the repository itself, kept by one hidden ref per turn, `refs/hutan/checkpoints/<session>/<turn>` (the end commit; its parent is the start); a turn that changed no file keeps no ref and is recorded as `unchanged`. They are taken through Hutan's own index, so your index, `HEAD`, branches, stash and working tree are never touched, and ignored files are never read. Files above `max_file_bytes`, past a snapshot's budget, with secret-looking names, LFS and other filtered paths, and nested repositories are left out and listed. The prompt waits at most 5 s for the start snapshot; past that it goes and the turn is marked `late` (its start may already hold some of its edits). A folder outside git, the home directory or a volume root, more than `max_untracked_files` untracked files, or no `git` record the turn as `unsupported` with the reason; three failed snapshots in a row or less than 2 GiB free switch the checkout's checkpoints `off` until a checkpoint setting is changed with `mode = "on"`. Mawas conversations and direct model chats have none.

Settings (`daemon.toml`, live; `hutan config set checkpoints.mode off` takes effect from the next snapshot):

```toml
[checkpoints]
mode = "on"                    # or "off"
max_file_bytes = 10485760      # 10 MiB
snapshot_budget_bytes = 67108864   # new content per snapshot, 64 MiB
max_files = 5000
max_untracked_files = 20000
exclude = [".env*", "*.pem", "*.key", "*.p12", "id_rsa*", "id_ed25519*", "*.kdbx", "credentials*"]
keep_turns = 100               # retention, per session
keep_days = 30                 # a session's checkpoints go after 30 days without a new one
max_repo_bytes = 1073741824    # 1 GiB recorded per repository
max_repo_refs = 1000

[checkpoints.projects.web]     # a project (name or id): mode and the four snapshot bounds
mode = "off"
```

Retention drops the oldest checkpoints first (a restore's, the state Undo needs, after every turn's), deletes their refs and packs `refs/hutan/*`; Hutan never runs `git gc` or `prune`, so their objects age out under git's own settings. `git log --all` shows checkpoint commits and `git push --mirror` would push them: do not mirror-push a repository Hutan checkpoints. The rows (`checkpoints` table) and the refs are reconciled when the daemon starts. Listing, diffing and restoring checkpoints come with CK-3 and CK-4.

## Configuration

`daemon.toml`:

```toml
[codex]
bin = "/Users/me/.local/bin/codex"                    # default: `codex`, found as described below
args = ["app-server", "-c", 'model_reasoning_effort="low"']   # default: ["app-server"]
```

**How agents are found** (Codex, OMP, Claude Code and its ACP adapter alike): `bin` when set, else the name searched along the `PATH` of the user's login environment, then the daemon's own `PATH` (inherited from whoever started it — a terminal's full `PATH` when `hutan` autostarted it from a shell), then the folders installers use: `~/.bun/bin`, `~/.local/bin`, `~/.cargo/bin`, npm/pnpm/Yarn user prefixes, Volta, mise/asdf shims, the newest nvm/fnm Node, Nix, `/opt/homebrew/bin`, `/usr/local/bin`. The agent then runs in the login environment with its own folder at the front of `PATH` (a `#!/usr/bin/env node` script finds the Node next to it). So a daemon started by Hutan.app, whose `PATH` is the short system one, finds agents however they were installed. `CODEX_HOME` is inherited from the daemon's environment.

## CLI

```text
hutan chat new [--agent codex|omp|claude] [--cwd DIR] [--model M] [--mode MODE] [--approval-policy untrusted|on-request|never] [--sandbox read-only|workspace-write|danger-full-access] [--ws W] [--title T]
hutan chat send <session> "<text>" [--wait]   # --wait streams the reply; tool calls are one line each; exit 1 if the turn fails or is cancelled
hutan chat log <session>                      # the journal as text (--json: the history)
hutan chat attach <session>                   # recent history, then live events until the session ends
hutan chat interrupt <session>
hutan chat models [--agent codex|omp|claude]
hutan chat model <session> <model>             # switch the model of a direct or harness chat (next turn)
hutan account ls | login <omp|claude|codex> [provider] [--attach] | logout <harness>   # § ACP harnesses and accounts
hutan approvals                               # pending requests (--json)
hutan approve <approval-id|prefix> <option>
```

`--json` prints events as JSON lines (`send --wait`, `attach`) or the protocol result.

## Tests and fixtures

Real Codex never runs in the default gate. Tests run against **`hutan-codex-fixture replay`**, a fake `codex app-server` that replays a recorded transcript (`crates/agents/tests/fixtures/codex-0.162/*.jsonl`, one `{"dir": "out"|"in", "msg": …}` object per line, in the order observed). Requests are matched by method; a response gets the id of the request it answers; after `thread/resume` the recorded thread id is rewritten to the requested one; a mismatch exits 3 with a message. A directory instead of a file serves one transcript per spawn (restart tests); `--received FILE` logs what the client sent, which the tests assert on (e.g. that the provider got exactly the chosen decision, or `thread/resume` with the stored thread id).

A transcript entry `{"dir": "hold", "file": PATH}` pauses the replay until `PATH` exists (at most a minute): the tests that need a stream to stand still half way (a window opened mid-stream) put one between two deltas and create the file when they are ready.

Fixtures: `plain-turn`, `approval-touch` (command approval), `approval-decline`, `approval-interrupt` (approval pending when the turn is interrupted), `file-change` (patch approval with default options), `interrupt`, `turn-failed`, `resume-turn`. Tests: adapter mapping per notification kind (`crates/agents/tests/it/mapper.rs`), the adapter against the fake server (`tests/codex.rs`: approvals, non-offered options, expiry, resume, version refusal, unknown messages, process death), the daemon end to end (`crates/daemon/tests/it/chat.rs`: lifecycle, approvals across clients, journal, restart), the CLI (`crates/cli/tests/it/chat.rs`), storage, and golden fixtures for protocol 0.9.

### Re-recording (new Codex version, new scenario)

Use synthetic prompts, a temporary working directory, and a temporary `CODEX_HOME` holding a *copy* of the login (delete it afterwards); never record from your real `~/.codex`.

```sh
# 1. compare the schema the installed Codex generates with what wire.rs expects
codex app-server generate-json-schema --out /tmp/schema     # and generate-ts

# 2. record: point a throwaway daemon's codex at the proxy, then drive it with the CLI
cat > $HUTAN_HOME/daemon.toml <<'EOF'
[codex]
bin = "/path/to/target/debug/hutan-codex-fixture"
args = ["proxy", "/tmp/raw.jsonl", "--", "codex", "app-server"]
EOF
HUTAN_HOME=… CODEX_HOME=… hutan chat new --cwd /tmp/scratch --approval-policy untrusted
HUTAN_HOME=… CODEX_HOME=… hutan chat send <id> 'Run `touch hi.txt` in a shell, then reply done.' --wait   # approve from another shell
# the proxy rewrites /tmp/raw.jsonl at every spawn: stop after the scenario you want

# 3. scrub (refuses output that still looks private) and commit
hutan-codex-fixture scrub /tmp/raw.jsonl crates/agents/tests/fixtures/codex-0.162/<name>.jsonl
```

The scrubber drops timestamps and rate-limit notices, renames every id to a stable counter, replaces temp directories, the Codex home, host name, account plan and installation id, and fails on home directories, e-mail addresses, API keys and tokens. A test asserts the committed fixtures are already scrubbed (scrubbing changes nothing). For a new Codex minor version, update `SUPPORTED_VERSIONS`, re-record the fixtures, and run `just check`.

## ACP harnesses (OMP, Claude Code) and accounts

ADR 0015, protocol 0.17 (capabilities `harness.acp`, `accounts`). An agent that speaks the [Agent Client Protocol](https://agentclientprotocol.com) v1 over stdio runs behind the same `AgentAdapter` as Codex: its messages become the same `AgentEvent`s, so the journal, `chat.history`, approvals, notifications, the history index, tasks and the chat view work unchanged.

| Where | What |
|---|---|
| `crates/agents/src/acp/mod.rs` | `AcpAdapter`: handshake, sessions, prompt turns, cancel, permission requests, models, modes |
| `crates/agents/src/acp/mapper.rs` | `session/update` and requests of the agent → `AgentEventDraft`s (pure) |
| `crates/agents/src/acp/nodes.rs` | tool calls that start subagents → the agent tree (pure) |
| `crates/agents/src/acp/udiff.rs` | a unified diff of ACP's old/new file contents |
| `crates/agents/src/jsonrpc.rs` | the transport; `spawn_v2` adds `"jsonrpc": "2.0"`, `request_ordered` delivers a response in order behind the updates before it |
| `crates/daemon/src/harness.rs` | the registry: where `omp` / `claude` are, their environment, `[mcp.*]` servers |
| `crates/daemon/src/accounts.rs` | `account.list\|login\|logout` |
| `crates/agents/src/bin/acp_fixture.rs`, `tests/fixtures/acp-omp-18.11/` | the fake agent and its transcripts |

### The adapter

`AcpAdapter::start(config, spec, mode, mcp_servers)` spawns the agent and:

1. `initialize` with protocol version 1. Hutan offers **no file system and no terminal** (`fs.readTextFile/writeTextFile: false`, `terminal: false`): the agent uses its own tools, Hutan's approvals gate them. An agent answering another `protocolVersion` fails the start with both versions named (`UnsupportedAcp`).
2. The session: `session/new {cwd, mcpServers}`, or — with a stored session id — `session/load` (else `session/resume` when only that is advertised; an agent with neither refuses with a message). The replay `session/load` makes is dropped: the Hutan journal is the history. MCP servers over `http`/`sse` are passed only when the agent advertises that transport.
3. `authenticate`, **only when `session/new|load` answers "authentication required"** (`-32000`) and the agent advertised a plain method (OMP's `agent`: the credentials the harness already holds). A method that needs a terminal or an environment variable is the harness's own login flow: the start fails with `AuthRequired` and the daemon's message says `hutan account login <harness>`.
4. The requested model must be one of the session's offered models — the `model` select option of `configOptions`, else the older `models` state — and is set with `session/set_config_option` (`session/set_model`); a requested `mode` must be one of `modes.availableModes` (or the `mode` option) and is set with `session/set_mode`. Unknown ones fail with what is offered.

A turn is `session/prompt` (text, plus attached context as embedded `resource` blocks when the agent advertises `embeddedContext`, else as text), made with `request_ordered`: its response arrives behind every update of the turn, which is what ends it (`stopReason`: `end_turn`/`max_tokens`/`max_turn_requests` → `turn_completed` — the last two with a warning `notice` —, `cancelled` → `turn_cancelled`, `refusal` → `turn_failed`; an error response → `turn_failed`). `interrupt` is `session/cancel` followed by `cancelled` answers to the permission requests still waiting (ACP requires them); the process dying under the prompt is left to the daemon, which closes the turn with the stderr.

### What becomes what

| ACP | `AgentEvent` |
|---|---|
| a turn (`session/prompt`) | `turn_started`, the user's `message_start`/`message_end`; `turn_completed` / `turn_cancelled` / `turn_failed` |
| `agent_message_chunk`, `agent_thought_chunk` (runs, split by `messageId`) | `message_start` / `message_delta` / `message_end`, `reasoning_start` / `reasoning_delta` / `reasoning_end` (a run ends when anything else happens, or with the turn) |
| `tool_call`, `tool_call_update` (`kind`, `title`, `rawInput`, `status`, `content`, `rawOutput`) | `tool_start` (`execute` is a command, `edit`/`delete`/`move` a file change, `fetch` a web tool), `tool_update` (new text content), `tool_end` (`completed`/`failed`, output cut at 256 KiB, exit code from `rawOutput`); a tool that never ended when the turn did fails with it |
| tool content of type `diff` (`path`, `oldText`, `newText`) | `file_diff` (an added, deleted or updated file; updates carry a unified diff made from the two texts, once per distinct diff) |
| `session/request_permission` | `permission_requested`, `options` exactly as offered (`optionId` → `id`, `name` → `label`, `kind`); answered with `{outcome: selected, optionId}`; `permission_resolved` when the answer reaches the agent, `expired` when the turn ends first |
| `plan` | `plan` (steps with status) |
| `usage_update` (`used`, `size`, `cost`), the prompt result's `usage` | `usage` (context window; last-request counts) and the tree node's usage `acp.context` |
| `current_mode_update`, `config_option_update` (model, mode) | `notice` (`mode: …`, `model: …`) |
| `available_commands_update`, `session_info_update`, `user_message_chunk` | dropped |
| any other update or notification | `opaque { method }` with the raw `payload` |
| requests Hutan does not serve (`fs/*`, `terminal/*`, …) | declined (`-32601`) and noted as a `notice` |

**Agent tree.** The root node is the agent (`source: acp`, node id the session id): `working` during a turn, `blocked` while a permission request waits, `idle` between turns, with the tool call's title as its activity. ACP has no subagent events, so subagents are recognised by their tool call: OMP's `task` tool (`rawInput {agent, task, name?, solutionSpace}` or `{tasks: [...]}` — one node per task) and Claude Code's `Task` (`subagent_type`, `description`); the call's `completed`/`failed` status ends them, a turn that ends first leaves them `interrupted`.

**Mode (protocol 0.34, `UX-MODE-CHIP`).** The mode the harness reports is the chat's `ChatInfo.mode`: at the start from `session/new` (`modes.currentModeId`, else a config option of category or id `mode`; OMP 18.11 reports `default` / `plan`), afterwards from `current_mode_update` and `config_option_update` (the adapter's `AdapterMessage::Mode`, besides the journal's `mode: …` notice), with the name the harness gives it. `approves_everything` marks a mode that lets the agent act without asking (`bypassPermissions`, `yolo`, `full-access`). The chat header shows `Mode: <name>`, the warning colour and `· approves everything` for such a mode, and `Mode unknown` when the harness reports none (Codex chats keep their approval policy and sandbox in the header instead). **Hutan never changes the mode of an ordinary chat** (user decision D4): it starts the harness bare (`omp acp`, `claude-agent-acp`; pinned by `harness.rs::ordinary_harness_chats_launch_bare_and_never_change_the_mode`) and sends no `session/set_mode`; only Mawas's own profiles set one (`docs/control.md` § Mawas), and `chat.create`'s `mode` when the user names one.

### Harnesses and configuration

| Agent | Starts | Models | Resume |
|---|---|---|---|
| `omp` (first-class) | `omp acp` | the ACP session's model option (ids `provider/model`); `chat.models` asks a throwaway session and, when that cannot be had, `omp models --json` | `session/load` of the OMP session id (`omp --resume <id>` in a terminal) |
| `claude` | `claude-agent-acp`, else the older `claude-code-acp` (Claude Code's ACP adapter; Hutan points to an installed one and does not bundle or download it: `npm install -g @agentclientprotocol/claude-agent-acp`) | the session's model options (`chat.models` starts a throwaway session in the system temp directory, asks, closes it; a running chat's start refreshes the cache too; cached 5 minutes. A cold OMP takes several seconds to answer `initialize` + `session/new`) | `session/load` / `session/resume` |

```toml
[harness.omp]                     # all keys optional
bin = "/Users/me/.bun/bin/omp"    # default: `omp`, found like Codex (see § Configuration)
args = ["acp"]                    # default for omp; none for claude
env = { OMP_PROFILE = "work" }    # extra environment; not for secrets
cli = "/Users/me/.bun/bin/omp"    # the harness's own CLI for accounts; default `bin` (omp) / `claude`

[harness.claude]
bin = "/opt/homebrew/bin/claude-agent-acp"

[mcp.docs]                        # handed to every harness session
command = "npx"
args = ["-y", "some-mcp-server"]
[mcp.search]
url = "https://mcp.example.com/mcp"   # `transport = "sse"` for SSE
```

The servers are managed without editing the file: Settings → Agents & tools → MCP servers (add, edit, remove), or `hutan config set mcp.docs '{command = "npx", args = ["-y", "some-mcp-server"]}'` / `hutan config set mcp.search '{url = "https://mcp.example.com/mcp"}'` and `hutan config reset mcp.docs` to remove one (`daemon.settings.set`, `docs/protocol.md`). The daemon checks the same rules as at startup (exactly one of `command` and `url`, `transport` only with `url`, a name of 1 to 32 lowercase letters, digits, `-` and `_`) and writes a `[mcp.<name>]` table; comments elsewhere in the file stay. The harness sessions get the change after the daemon restarts.

Harness processes start from the user's **login environment** (as terminal sessions with an explicit command do; `[terminal] env_inherit = "daemon"` uses the daemon's own), so a GUI-launched daemon finds `~/.bun/bin/omp` and the harness finds its own configuration and logins. Hutan reads none of that. A missing program fails the chat with the name looked for, what to install and the `daemon.toml` key to set. `approval_policy` and `sandbox` do not apply: the harness decides what asks and what is sandboxed (OMP: its `tools.approvalMode`), and `chat.create` refuses them for these agents rather than show Hutan's defaults as if they did. The session mode (`--mode plan`) is the harness's own.

**Restore.** The `ChatSpec` stores the ACP session id (`thread_id`, also `ChatInfo.agent_session_id`) and the mode. After a daemon restart (`restore_sessions = "respawn"`) or `hutan restart <id>` the agent starts again, continues the stored session and the journal is untouched; an agent that cannot (`session/load` fails, or neither it nor `session/resume` is advertised) fails the session with the agent's message.

### Accounts

`hutan account ls|login <harness> [provider]|logout <harness>` (`account.list|login|logout`). **Hutan never performs subscription OAuth and never reads, stores or forwards harness tokens** (ADR 0008, ADR 0015): it asks each harness what it is logged in as, and for logging in or out it opens the harness's own flow in an ordinary terminal session.

| Harness | List | Login | Logout |
|---|---|---|---|
| `omp` | the providers of `omp models --json` (a provider with usable models has a login), plan from `omp usage --json` (`metadata.planType`) | `omp login [provider]` | none exists: a session running `omp`, `instructions` say to type `/logout [provider]` |
| `claude` | `claude auth status --json` (`loggedIn`, `authMethod`, a plan field when there is one) | `claude auth login` | `claude auth logout` |
| `codex` | the app server's `account/read` (`chatgpt`, `apiKey`, `amazonBedrock`; plan) | `codex login` | `codex logout` |

Entries carry provider, a label (`ChatGPT`, `API key`, `Claude subscription`, the provider id for OMP), plan, status and, when the harness could not be asked, a `detail`. E-mail addresses, account ids and token-like strings are never read into an entry. A login/logout session ends like any session; the daemon then publishes `account.changed {harness}` and clients list again. `hutan account login omp anthropic --attach` attaches the terminal to the session at once.

### Tests and fixtures

`hutan-acp-fixture replay` is the fake `omp acp`: the replay engine of `hutan-codex-fixture` (transcript entries `{"dir": "out"|"in"|"hold"|"exit", …}`, requests matched by method, responses given the id of the request they answer, a directory = one transcript per spawn, `--received FILE`), with its own scrubber (`acp_scrub`: session ids become `sess_fixture_N`, temp and home directories and e-mail addresses are replaced, tokens refused; a test asserts committed fixtures are already scrubbed).

`crates/agents/tests/fixtures/acp-omp-18.11/`: `handshake.jsonl` was **recorded from `omp acp` 18.11.1** (initialize, session/new, session/close; run with a throwaway `HOME`, so no login was involved and no model was called: the 17 models are OMP's built-in catalog). The others are built by hand from the ACP v1 spec and what OMP's ACP server sends (message shapes read from its source: tool kinds, permission option ids, `usage_update`, config options), reusing the recorded handshake: `plain-turn`, `tool-diff`, `permission-allow|deny|cancel|expires`, `cancel`, `unknown-update`, `subagents`, `model-switch`, `mode-plan`, `auth-required|terminal-only|rejected`, `old-protocol`, `load`, `resume-only`, `no-resume`, `load-fails`, `exit-mid-turn`, `turn-error`, `refusal`, `max-tokens`, `two-turns`. A real prompt turn was not recorded: it needs a login and paid tokens, which the default gate and this slice do not use.

Tests: the adapter against the fake agent (`crates/agents/tests/it/acp.rs`), mapping units (`acp/{udiff,nodes}.rs`), the daemon end to end with fake harness CLIs as shell scripts (`crates/daemon/tests/it/harness.rs`: create/send/approve/interrupt/restore, models, attachments, tasks, accounts list/login/logout/`account.changed`), the CLI (`crates/cli/tests/it/harness.rs`), golden fixtures for protocol 0.17.

### Re-recording (new OMP version, new scenario)

Record with a throwaway `HOME` (no credentials involved) for what needs none; anything that needs a login is built by hand after a recording of the message shapes. Use a synthetic prompt and a temporary working directory, never your real `~/.omp`. A small script writes `initialize`, `session/new` and `session/close` (JSON-RPC 2.0, one object per line, `protocolVersion: 1`) to the proxy's stdin and reads its stdout:

```sh
HOME=/tmp/throwaway hutan-acp-fixture proxy /tmp/raw.jsonl -- omp acp
hutan-acp-fixture scrub /tmp/raw.jsonl crates/agents/tests/fixtures/acp-omp-18.11/handshake.jsonl
```

Compare a new OMP's `initialize` (`protocolVersion`, `agentCapabilities`, `authMethods`) with the recorded one; Hutan accepts any agent that answers protocol version 1.

## Rich input

Protocol 0.23, capability `chat.rich_input`. Files, images and pasted text can be attached to any chat. Delivery: ACP harnesses get text attachments as `resource` blocks (quoted text without `embeddedContext`) and images as `image` blocks when the handshake offers `promptCapabilities.image`, else the message is refused with `unsupported`. Codex gets text attachments quoted after the message and images as `localImage` input items; a clipboard image is written to `$HUTAN_HOME/tmp/img-*` and deleted when the turn ends (stale files are swept at daemon start). The user's `message_end` carries `attachments_meta` for every adapter (image bytes are never journaled: path and SHA-256 only). Images are not redacted. The desktop composer takes drops anywhere on the chat, ⌘V of Finder files and clipboard images, long pastes (over 2 000 characters or 40 lines) folded into a `Pasted · N lines` chip, and `+` → "Attach file or image…"; chips that cannot be sent say why before Send. CLI: `hutan chat send <session> --attach file:<path> --attach image:<path> "text"`. Limit: GPUI reads a clipboard with text before one with an image, so an image copied together with text pastes as text.

## The desktop chat view

`⌘⇧T` opens the **Start…** sheet (`docs/design.md` § Start…), whose Agent row lists the harnesses — OMP first, then Claude Code, Codex — each with its detection (installed / not found and how to install it, from `account.list` and the model query) and the providers it is logged in to; one that is not found cannot be picked. The preselected harness is the first that is not missing (until you pick one). The model list is the harness's `chat.models` (OMP's grouped by provider; a harness that offers none starts with its own default). The chat header shows `harness · model`, a **context chip** and **Switch model…** (between turns) works for OMP and Claude Code as for direct chats (`chat.set_model`); a Codex chat keeps its model. **Accounts** (⌘K *Accounts…*, Settings → accounts) lists each harness with its accounts (provider, label, plan, status; never a token) and **Log in** / **Log out**, which start the harness's own flow (`account.login` / `account.logout`) and show the terminal session the daemon opened; it refreshes on `account.changed`. With no harness installed the page shows the mascot and says so. The desktop then asks for a model from `chat.models`, starts the chat in the active workspace's first folder (`chat.create`) and opens it as a **chat tab**: same tab/split/move rules as terminals, the tab icon `◆`. Code: `crates/chat_view` (GPUI-free: `Transcript` folds `AgentEvent`s into entries, the markdown subset, unified diffs) and `crates/desktop/src/workbench/{agents.rs,chat/}` (model, view, cards, composer).

- **Transcript**: virtualized list. User and assistant messages (markdown: paragraphs, emphasis, inline code, fenced code with a Copy button, lists, quotes, links; no HTML, no images; tables stay preformatted), reasoning (folded, "Thinking…" while it streams), command cards (command, cwd, live output tail, exit code, click to show all), file-change cards (files with +/− counts, click a file for its unified diff; shown automatically while an approval waits), other tools (name, arguments folded), plans as a checklist, a footer per turn (outcome, duration, tokens, context use).
- **Jump to latest** (UX-JUMP-LATEST, chat half): the list follows the end of the conversation until the reader scrolls up; from then on streaming text and new cards never move the view (the reader's place is kept), and a pill at the bottom of the transcript says what arrived below — `3 new ↓` (entries added since the reader left the bottom; those put in front by *Load earlier* do not count) or `New output ↓` while only the last entry grew. Following resumes only when the reader scrolls back to the bottom (the pill goes with it), when they click the pill (it returns to the end and follows again), or when they send a message. The terminal's pill is a separate card (`terminal_view`).
- **Context meter** (UX-CONTEXT-METER): the header chip `context 61 % · 122k / 200k` reads the latest usage event that reported a total — the same one the turn footers' `context X / Y` use (ACP `usage_update`, Codex `thread/tokenUsage/updated`), so it needs no protocol field. A harness that never reported a total has **no chip** (hidden, never a zero); one that reports a total but no window shows the muted `context 122k` (no percentage of an unknown window); under 1 % reads `<1 %`. The tooltip gives the exact figures and the cumulative input / cached / output / reasoning counts the harness reported, nothing it did not. **Cost is not shown**: ACP's `usage_update.cost` is not carried by `AgentEventKind::Usage` today, and Hutan keeps no price table (`docs/spikes/2026-10-20-chrome-v3.md` §2.7). Mawas's own header row has no chip.
- **History and follow-up**: the newest 200 events of the journal (`chat.history`, `HISTORY_DEFAULT_LIMIT`) are read when a tab opens, then `chat.event`s are applied. Every event is applied once (by `seq`), a reconnect reads the journal again and applies only what is new, a daemon restart (new epoch) starts the transcript over. Text that streamed while the window was away is complete again when the item ends, and a window opened mid-stream starts from the journal's snapshot of the open message and continues with the live deltas after it (`Transcript` keeps the `seq` its text includes: a delta already in a snapshot is not added twice, one that came live before the snapshot is put back after it).
- **Load earlier**: while the journal has events before the first one shown, a row above the transcript says so with a **Load earlier** button, and scrolling to the first entry does the same: the page before the oldest `seq` read (`before_seq`, 200 events) is put in front of the transcript (`Transcript::prepend`: an event seen before is skipped, an item that continues across the page boundary is completed in place, the turn in progress and the usage of its footer are not touched) and the list keeps the entry the reader was at. Paging continues until `has_more` is false. The list is virtualized, so only visible entries are laid out; the loaded entries are all in memory.
- **Composer**: the send key (`chat.send_key`, default `cmd-enter`) decides: `⌘↩` sends and `↩` is a newline, or with `enter` `↩` sends and `⇧↩` / `⌥↩` are the newline; `⌘↩` sends under both. A `↩` that commits an input method's marked text never sends. The hint under the composer (`⌘↩ send · ↩ newline` / `↩ send · ⇧↩ newline`) is a control: clicking it opens a two-item menu that writes the setting, and every open composer follows at once; ⌘K **Chat: Toggle Send Key** does the same with a short notice. Once setup is done, the first composer that gets the keyboard also asks the question in one line (the two choices and a ×; `chat.ask_send_key` stops it). The same key rules the Start sheet's prompt, review comments and "send review", and free-text answers in question cards (`docs/design.md` § Send key). `⌘.` or the Interrupt button stops the turn; the composer is disabled while the session starts, ended, or the connection is down (Restart button for ended sessions).
- **Approvals**: the request sits on its tool's card with one button per option the agent offered, in the agent's order (keys `1`–`4` answer the oldest request while the composer is empty; Return and a click beside the buttons answer nothing). Only the choices about this one request — *allow once* and *reject once* — are primary buttons (on the card and in the inspector's Requests tab alike: `ui::approval_button_kind`); "Always" options (`allow_always`, `reject_always`, Codex's session and exec-policy prefix options), *cancel* and the agent's own kinds are secondary, so an agent that lists "Always allow" first never makes a standing rule the easy click. Under the buttons, each "Always" option has a line: `<n> <label>: beyond this one request, <harness> stops asking about what its rule for “<tool>” covers. Hutan cannot show or revoke that rule.` (or `refuses, without asking,` for a reject) — the label is the agent's own, the tool is the request's title (first line, at most 80 characters), and the rule is the harness's: Hutan lists and revokes its own grants only (`docs/control.md`). The card shows where the request would act and everything it touches, never clamped: the command in full, and for ACP harnesses a detail of `in <session cwd>` followed by every file the request names (`toolCall.locations` and `diff` content entries, each once; Codex's detail has had `in <cwd>` since M4); the Agents-page review shows the same detail unclamped. The same requests are in the inspector's Approvals tab, the status bar (`approvals` item), the sessions panel (state `blocked`, needs you) and ⌘K → *Agent: Review Approvals…*. A resolved card says what was decided and where the answer came from: "Allowed once · from this window", "· from CLI", "· from another window" (a desktop connection that is not this one), "· from a notification", "· from another client" (an answer journaled before protocol 0.15, or a kind this build does not know) — from the `decided_by` the daemon journals with the answer, so a window opened later shows it too.
- **Drafts** (UX-DRAFTS): what is typed in a chat's composer is kept per chat in `desktop-state.json` under `chat_drafts` (session id → text), so closing the tab, closing the window or quitting the desktop loses nothing; it is back in the composer when the chat opens again, and sending (or emptying the composer) removes it. Drafts are **local plaintext** — the state file is not a place for secrets (`AGENTS.md`): only composer text is kept (never a question's secret field, a provider key field or an attachment), each draft is cut at 32 KiB, and drafts of sessions the daemon no longer lists are dropped once the session list is known. Mawas's own composer on its page is not covered. Never sent to the daemon.
- **Send to agent**: the hover bar's button and the *Block: Send to Agent* command attach the block's command and output (cut at `chat.attach_max_bytes`, default 64 KiB) as a chip in the composer of the chat that was focused last (a menu asks when none was); the chip is quoted in a fenced block in front of the message when it is sent.
- Settings namespace `chat`: `send_key` (`cmd-enter` or `enter`; the earlier boolean `send_on_enter = true` still reads as `enter`, and writes go to `send_key`), `ask_send_key`, `attach_max_bytes`. `panels.inspector.reveal_on_approval` opens the inspector when a new request arrives.

Tests: `crates/chat_view` (unit), `crates/desktop/tests/it/chat_gpui.rs` (an in-process daemon over `hutan-codex-fixture replay`).

## Questions (protocol 0.24, capability `chat.questions`)

Agents sometimes ask the user something that is not a permission. Hutan answers those requests instead of declining them.

### What the agents actually send

Read from the installed packages (no model turn was run); each fact was checked against the schema or source named.

| Agent | Request | Shape |
|---|---|---|
| Codex 0.162.1 (`codex app-server generate-json-schema`) | `item/tool/requestUserInput` (server→client, EXPERIMENTAL) | params `{threadId, turnId, itemId, isBlocking, autoResolutionMs?, questions: [{id, header, question, isOther, isSecret, options: [{label, description}] \| null}]}`; response `{answers: {<question id>: {answers: [string]}}}`. There is no decline or cancel in the schema. |
| Codex 0.162.1 | `mcpServer/elicitation/request` | params `{threadId, turnId \| null, serverName, mode, message, requestedSchema?, url?, elicitationId?}`; `mode` is `form` (MCP 2025-11-25 `requestedSchema`: object of string / `enum` / `oneOf` of `{const, title}` / number / integer / boolean / array of enum), `openai/form` and `openaiForm` (any schema) or `url`; response `{action: accept \| decline \| cancel, content?: object}`. |
| Codex | `serverRequest/resolved {requestId}` | notification that ends either request (also after an interrupt). |
| OMP 18.11.2 (`src/modes/acp/acp-agent.ts`) | ACP `elicitation/create` (unstable), only when the client advertised `clientCapabilities.elicitation.form` | `mode: "form"`, `sessionId`, `message`, `requestedSchema`. OMP's `ask` tool (`askDialog`) sends fields `q<n>` (`oneOf` of `{const, title, description}`, `default` for the recommended option, or an `array` of `anyOf` for multi-select) each with a `q<n>__other` free-text field; `select`, `confirm`, `input`, `editor` and the plan-mode approval ("Approve plan … ?", an `enum` of two strings) send one field `value`. Without the capability OMP's `ask` returns nothing and plan approval auto-approves. |
| Claude Code through `@agentclientprotocol/claude-agent-acp` 0.67 (`dist/elicitation.js`) | `elicitation/create` for the `AskUserQuestion` tool (fields `question_<n>`: `oneOf`, or `array` of `anyOf` for multi-select, each with a `question_<n>_custom` free-text field marked in `_meta`), for MCP elicitations (form and url modes), and for the refusal-fallback consent prompt | the tool is disabled for clients that do not advertise `elicitation.form`. |
| ACP schema 1.3.0 | `CreateElicitationResponse` | `{action: "accept", content?} \| {action: "decline"} \| {action: "cancel"}`; content values are string, integer, number, boolean or string array. |

### What Hutan does

`question_requested` / `question_resolved` events join the journal; `question.list` and `question.answer` (protocol.md § Questions) are the requests. A question has a title, a markdown prompt and typed fields (`text`, `multiline`, `choice {options, multi, allow_other}`, `number`, `boolean`, `secret`) mapped from the provider's schema by `crates/agents/src/elicit.rs`; what has no mapping (a nested object, `$ref`, a non-object schema, Codex's `openai/form` modes) becomes one free-text field that takes the answer as a JSON object, with the schema shown beside it. A `requestUserInput` question with `isSecret` is a `secret` field; so is a string with `format: password` or `writeOnly`.

Replies are the provider's own: Codex `requestUserInput` gets `{answers: {id: {answers: [text]}}}` (a multi value is several strings); decline is an empty `answers` and cancel is the same plus `turn/interrupt`, because the schema has no better words; an MCP elicitation gets `accept {content}`, `decline` or `cancel`; ACP `elicitation/create` gets the same actions, and Hutan advertises `elicitation.form` (not `url`: a url-mode elicitation is declined with a notice). Answers are checked against the fields by the daemon (`bad_request` for an unknown field, a missing required one or a wrong type) and defaults fill the fields that were left out.

Lifecycle, like approvals: a question lives in the daemon, survives client disconnects, is answered once (`not_found` afterwards; the first answer to reach the agent is the one that counts), and ends as `answered`, `declined`, `cancelled` or `expired`: when its turn ends, when the user interrupts (ACP: the agent is told `cancel`), when the agent process exits, and when the daemon restarts (an elicitation outside any turn, `expires: process`, lives as long as the process). Requests and outcomes are stored in the `questions` table (never the answers) with `decided_by`, the answering client's kind, also on the event.

**Secrets.** The values of `secret` fields reach the agent and nothing else: the `question_resolved` event, the journal, the `questions` table, `question.resolved` and the logs say `withheld: ["token"]` ("answered (secret)"); `QuestionResponse` prints only field names in `Debug`; the CLI reads a secret from stdin (`field=-`); the card masks it and drops it with the form. A test scans every file of the daemon's home for the value.

### Desktop, CLI, notifications

A question is a card in the chat (`workbench/chat/question_card.rs`): the prompt, a control per field (option buttons or checkboxes, a free-answer box, a text box, a Yes/No toggle, a masked box for a secret), **Submit ⌘↩**, **Decline**, **Cancel**; Tab with an empty composer moves into the oldest card, Tab/⇧Tab walk the fields, Escape leaves. While a card has the keyboard typing is its field's, digits included; the `1–4` approval shortcut only ever answers approvals. The session shows `blocked · question`, counts in *Needs you*, the menu bar item, the Dock badge (`AttentionKind::Question`) and the daemon's own menu bar item (`Need::Question`); notification intent `question_required` (kind enabled by default under `[notify]`) offers *Open* only. The inspector's *Approvals* tab is now **Requests** (its id in `[inspector] tabs` is still `approvals`) and lists questions with a link to their chat. A question that **ends unanswered** (the turn ends, the user interrupts, the agent goes away) keeps what was typed: its card says *This question ended — copy your answers*, lists the typed fields (`label: text`) and offers **Copy**; a secret field is emptied the moment the question ends and never listed, and a form nobody typed into leaves nothing behind. The form lives in the chat view (not the state file): closing the tab drops it.

```text
hutan questions                                   # pending questions: id, session, title, fields (--json)
hutan answer <id|prefix> field=value ...          # repeat a field for a multi-select; secret: field=-  (value from stdin)
hutan answer <id|prefix> --decline | --cancel
```

### Tests and fixtures

`codex-0.162/question-*.jsonl` and `acp-omp-18.11/question-*.jsonl` are **hand-built from the schemas and sources above** (a real recording of `requestUserInput`, an MCP elicitation or `AskUserQuestion` needs a paid model turn) on top of the recorded handshake and turn of `approval-touch` / `permission-allow`, scrubbed with the same scrubbers (the Codex scrubber now keeps the `serverName` of an MCP elicitation). Tests: the schema mapping (`elicit.rs`), adapter mapping (`tests/mapper.rs`, `tests/codex.rs`, `tests/acp.rs`: reply shapes, decline/cancel, expiry on interrupt, process-scoped elicitations, urls), daemon lifecycle (`daemon/tests/chat.rs`: answer, validation, provenance, decline, expiry, restart, secret not in any file), the CLI (`cli/tests/chat.rs`) and the card (`desktop/tests/chat_gpui.rs`, `chat/question.rs`).

## Limits

Codex and the ACP harnesses (OMP, Claude Code; direct model chat has its own page, `docs/gateway.md`), one conversation per session, text input only (no images; terminal blocks are quoted into the text). What a harness exposes over ACP is what Hutan shows: an agent without model options has no model switching, OMP's subagent progress is inferred from its `task` tool calls (ACP has no subagent events), and Hutan as an MCP server for agents is not there yet. Questions of the agent (`requestUserInput`, MCP and ACP elicitations) are answered (§ Questions); url-mode elicitations are declined. History search over chat journals is not wired into the M17 index yet: the journal has user, assistant and tool rows with the shape the indexer needs (`source: hutan`).
