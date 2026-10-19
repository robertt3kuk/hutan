# Mawas, the assistant (roadmap M16: the daemon side; MW-1: Home, acting with safeguards; MW-2: memory)

Decisions: ADR 0017 (Mawas, the local personal assistant as a meta-harness; decision 4 is the contract for the safeguards), ADR 0008 (no secrets in context; credentials), ADR 0001 (the daemon owns conversations). Wire messages: `docs/protocol.md` § Assistant chat and context attachments, § Control. The direct chat it is built on: `docs/gateway.md`. The index it reads and feeds: `docs/history-index.md`. The safeguards in depth: `docs/control.md` § Mawas. The desktop page for it (Mawas Home) is described in § Desktop. *Assistant* is the internal name (`assistant.*` settings and requests, `hutan assistant …`); everything the user sees says **Mawas** (Malay for "orangutan").

**Mawas** is the assistant: it answers on the backend of the user's choice (OMP, Claude Code or Codex over their own agent loop, or a direct provider labelled *Chat only*), lives in a scope, starts from a system prompt that explains attachments, and can be sent **context**: shell output, recent terminal lines, the conversation of a coding agent, a file, search results. It also **acts**: in its default mode, **Ask before acting**, it reads freely and every change (its harness's own tools and Hutan's, through MCP) waits for the user's approval (§ Modes and safeguards); in **Advisor** mode it reads and advises only. The context is chosen by the user for each message, read by the daemon, redacted, bounded and journaled with the message. Nothing is read that was not attached.

## Parts

| Where | What |
|---|---|
| `crates/daemon/src/assistant.rs` | `AssistantService`: `assistant.open`/`assistant.list`, permission checks, the resolver of each attachment kind |
| `crates/daemon/src/context.rs` | the pure half: redaction, budget share, truncation, headers, assembly, turns of a conversation |
| `crates/daemon/src/context_file.rs` | reading a file below the workspace roots without following symlinks |
| `crates/daemon/src/{chat,direct}.rs` | `chat.agent = "assistant"` is a gateway chat; the message with its attachments is the user message of the turn |
| `crates/daemon/src/history.rs`, `crates/index` | chat journals feed the index; `IndexHandle::session_text` reads a session's rows |
| `crates/client`, `crates/cli` | `Client::chat_send_with`, `assistant_open`, `assistant_list`; `hutan ask`, `hutan assistant` |

## Scope and sessions

A session's scope follows from its workspace: the Home workspace holds the **global** assistants (they see the Home folder, nothing else), every other workspace its **workspace** assistants (they see the workspace's roots). `assistant.open { scope, workspace_id? }` returns the default assistant of the scope — the oldest assistant session of that workspace — and creates it on first use on the backend chosen for the scope (§ Backends: `assistant.configure`, `[assistant]` of `daemon.toml`, else the **automatic backend**: the first harness or provider that is ready). It fails with `invalid_state`, listing what it checked, only when nothing is configured and nothing is ready. A stopped default is started again. `new: true` (or `chat.create` with `agent: "assistant"` and a provider) makes one more; `assistant.list` shows them all and which is the default.

Sessions created in the same millisecond keep their insertion order, including after updates
and daemon restart. Restore must not use random ULID order as the tie breaker: otherwise
opening the default assistant could select a different conversation.

An assistant session is an ordinary chat session: `chat.send`, `chat.history`, `chat.interrupt`, `chat.set_model`, `session.stop`/`restart`, restore after a daemon restart all work as for a direct chat. Its system prompt is `assistant::SYSTEM_PROMPT`.

## Backends (protocol 0.20, M8-B)

The assistant is **optional** (desktop `assistant.enabled`, § Desktop) and chooses what answers it per scope: `omp`, `claude`, `codex`, or a direct `provider` (M5) with a model. The choice is `daemon.toml [assistant] backend = "omp" | "claude" | "codex" | "provider"` (+ `model`, + `provider` for the provider backend; `provider` alone still means the provider backend), overridden by `assistant.configure` — globally or for one workspace — which the daemon stores in its database. A project inside a workspace (protocol 0.19) uses the workspace's choice unless it has its own, then the global one; `assistant.open` and `assistant.configure` with `scope: workspace` take a workspace or a project id, and the assistant lives in the id it was opened for (a project's assistant sees that project's folders). `assistant.open` returns the oldest assistant of the scope on the configured backend, so switching backend keeps each backend's conversation and a second `open` after switching back finds the first again.

**Automatic backend (AM-1).** With nothing configured for a scope (no `assistant.configure` for the workspace, its parent or globally, and no `[assistant]` in `daemon.toml`) `assistant.open` runs the first backend that is *ready*, in this order:

| Backend | Ready when |
|---|---|
| `omp` | OMP is found and at least one provider is logged in (`omp models --json` lists models) |
| `claude` | Claude Code is logged in and its ACP adapter is found |
| `codex` | Codex is logged in |
| `provider` | the first provider of `hutan provider ls` that has an API key, or needs none: Ollama, and an OpenAI-compatible server on this machine (loopback); its model is the provider's default model, else the pick below over `provider.models` |

Readiness is asked lazily, on `assistant.open` only, and kept for 30 seconds (dropped when an `account login`/`logout` session ends), so opening assistants does not start every harness's CLI each time; a harness that does not answer within 20 seconds is not ready. Providers are read from the daemon's own configuration each time. When none is ready `assistant.open` fails with `invalid_state` and one reason per candidate (`omp: …; claude: …; codex: …; provider: …`). The assistant that exists stays the default of its workspace (the oldest one) whatever is ready later; `AssistantEntry.backend` is what the session runs on.

**Automatic model (AM-1).** A backend with no model (the setup stores `backend` without one; so does `daemon.toml` without `model`; the automatic backend has none) would run the harness's own default, which for OMP can be the most expensive tier. For a read-only helper that answers questions the daemon picks from the backend's catalog (`chat.models` for a harness, `provider.models` for a provider) when it creates the session, with `assistant_pick::pick_assistant_model`: the catalog's default (else the first model that is not hidden), moved **one tier down** when it is a premium tier and the catalog has the next tier of the same vendor:

| Premium default | Replaced by | Why |
|---|---|---|
| Anthropic `opus` | the newest `sonnet` | Opus costs several times Sonnet; Sonnet answers questions as well |
| OpenAI `*-pro` (`o3-pro`, `gpt-5-pro`) | the same family without `-pro` (`o3`, `gpt-5`) | the Pro variants are priced far above the base model |
| Google `*-ultra`, Deep Think | the newest `*-pro` | Pro is Google's ordinary tier |

The id, the display name and (when those name no tier) the description are matched case-insensitively: Claude Code's ACP adapter offers `default`/`opus`/`sonnet`/`haiku` with the real model in the description, OMP ids are `provider/model` and a replacement never leaves the default's provider prefix (`anthropic/…opus…` becomes `anthropic/…sonnet…`, not another provider's). *Newest* is the highest version in the id (`4-5` over `4-1` over `4`), then the latest date (`20250929` over `20250514`); ties keep the catalog's order. A default that is not premium stays. The picked id is the session's model (`chat.model`). A model that was stored or configured always wins and no catalog is read for it. An empty, unreadable or slow (20 s) catalog means no pick: the harness's default runs, the open does not fail. The table is fixed in the code: no network and no model call decide it.

What each harness assistant may do, by mode (restricted or steered where the harness allows it, never faked; `assistant.mode`, § Modes and safeguards):

| Backend | Advisor | Ask before acting (default) |
|---|---|---|
| `omp` | `omp acp --tools read,grep,glob --append-system-prompt <read-only prompt>`: no `bash`, `edit`, `write`, `task`, web | `omp acp --approval-mode always-ask --append-system-prompt <Mawas prompt>`: all tools; everything above reading asks the client (`session/request_permission`), whatever the user's own OMP configuration says |
| `claude` | Claude Code's ACP session in its `plan` mode | its `default` permission mode: it asks (`session/request_permission`) before it edits or runs |
| `codex` | `codex app-server` with `sandbox: read-only`, approval policy `never` (a refused write fails inside the sandbox) | `sandbox: workspace-write`, approval policy `on-request`; Codex's approvals reach Hutan's cards |
| `provider` | the direct chat gateway: no tools | the same: **Chat only** until the gateway has a tool loop (ADR 0017, MW-8) |

The permissions below (`[assistant.context]` …) gate attachments for every backend. A harness assistant's session is marked `ChatInfo.assistant`; it is an ordinary harness chat otherwise (journal, restore, history index). Harness assistants need the harness's login (`hutan account ls`); when the harness is missing or logged out `assistant.open` fails with the harness's own message (when it was chosen; the automatic backend only picks a ready harness).

**Resetting a scope.** `assistant.configure` stores a choice per scope and there is no such thing as "automatic" among its backends, so a choice made once would stay. `assistant.reset {scope, workspace_id?}` (protocol 0.28; `hutan assistant reset [--ws ID]`) removes the stored choice of that scope (the `app_meta` key it was written to): the scope inherits again, workspace → its parent workspace → the global choice → `[assistant]` of `daemon.toml` → the automatic backend. Assistant sessions that exist are untouched and `assistant.open` follows the inherited choice from then on (the oldest assistant that runs on it is the default, one is created when there is none). Resetting a scope with nothing stored is not an error. Like `daemon.settings.set` it is user-only: an agent-originated connection gets `forbidden` under every `[control]` policy. The `[assistant]` defaults (`backend`, `provider`, `model`, `context_budget_chars`, `idle_stop_secs`) and the permissions are also settings of `daemon.settings`, live.

## Modes and safeguards (protocol 0.31, MW-1; ADR 0017 decision 4)

Mawas makes tool calls, and the daemon decides what they may change. `assistant.mode` (`daemon.toml` `[assistant] mode`, a live daemon setting, Settings → Mawas, the mode chip of Mawas Home) chooses between two modes:

- **Ask before acting** (default). Reading (files, history, sessions, status) runs freely. Every **change** raises an approval in Hutan — in the chat, in the *Today* rail of Mawas Home, in `hutan approvals` and as a native notification — with **Allow once / Allow for this chat / Always allow this tool in this workspace / Deny**. This covers Hutan's own tools (the `hutan mcp` server every Hutan-started chat gets: sessions, worktrees, tasks, workspaces, `ui_run`, …) and the harness's native ones (edit a file, run a command: its `session/request_permission` or Codex approval reaches the same cards).
- **Advisor**. Mawas reads and advises; the harness runs with its read-only tools and every mutating Hutan request is refused with the reason ("Mawas is in Advisor mode …").

What stays the user's whatever the mode: Mawas can never answer an approval (its own included), create, widen, list or revoke a grant, change a daemon setting (`daemon.settings.set`, `assistant.reset`, the mode), or touch provider credentials and logins. Destructive or irreversible changes (kill or stop a session, close one that runs something, delete or remove a workspace, a worktree, a task, merge, type into a terminal, stop the daemon, wipe the history index, choose what runs the assistant, quit the window) **always ask**: no grant covers them. At most `assistant.max_actions_per_turn` (default **10**) changes run per turn without asking; the next one asks ("Mawas has made 10 changes this turn"). **Stop** on Mawas Home is the kill switch: it denies what the chat has pending and interrupts its turn. Every decision is audited (`hutan::audit`, the `approvals` table) and shown in the chat on the tool's own card with its outcome.

The enforcement lives in the **daemon** (origin attribution of Mawas sessions, `ControlService::gate`), not only in the harness flags; `docs/control.md` § Mawas has the full rules, the grant tables and the limits of origin attribution (a marker for well-behaved agents, not a boundary against a hostile one). A change of mode applies to the **next turn**: the daemon restarts the harness session for it (the stored conversation continues) and the journal says so; the gate follows the new mode at once.

**What is sent to the model.** Everything stays on the daemon's host, but what a turn selects is sent to the backend's model: the user's message, the context attached to it (redacted and bounded, § Attachments), the conversation so far and — because Mawas can act — what its tools return (file contents, command output, Hutan's answers). Nothing is attached or read that the user did not choose, except what Mawas itself reads with its tools in Ask before acting mode; Advisor keeps that to the read tools of the harness. Mawas Home says so under the composer's empty state.


## Memory (protocol 0.32, MW-2; ADR 0017 decisions 3 and 4)

Mawas remembers what the user approved — about the user (**profile**), about environments and projects (**fact**) and per-workspace decisions (**note**) — locally, with history, and nothing a model writes is "saved" without the user's approval and a daemon receipt. Wire messages: `docs/protocol.md` § Memory; the rules for agents: `docs/control.md` § Memory.

**What is stored.** `memory_entries` (id, scope `global` | `workspace` + `workspace_id`, category, `current_revision`, state `active` | `archived`), `memory_revisions` (immutable: body, SHA-256 `content_hash`, `provenance_json`, `author_kind` `user` | `agent`, `approval_id`, a unique `operation_id`, the scope revision it produced; triggers refuse to change or delete one), `memory_proposals` (what an agent asked for and where it stands: `pending`, `applied`, `rejected`, `conflict`, `cancelled`), a revision counter per scope (`memory_scopes`) and `memory_snapshots` (exactly what a conversation's backend was given, with a manifest of entry ids and revisions); storage migration 0013. Archiving and restoring add revisions; nothing is erased in this version, so history stays. An entry belongs to the scope it was written in: another backend, another chat, or a model's opinion never moves it between scopes.

**Rules (daemon, fail closed).** A body is at most **2 KiB**; at most `assistant.memory_max_entries` (default **200**, live) entries are active, and a write or a restore past it is refused until one is archived. A body that the `hutan_index` redactor would change (key, token, password and private-key shapes) is **refused** with "Memory is not a secret store" — it is never masked and stored — for the user's write and an agent's proposal alike (the detector is a heuristic: a harmless string shaped like a key is refused too; rephrase it). **Provenance** is stamped by the daemon (the user's client, or the proposing session and turn), never taken from the request. A revision a model wrote (a proposal approved as it was) is **inferred** until the user edits or confirms it (saves it unchanged); *Edit & approve* makes the user the author.

**Proposals.** An agent proposes with `memory.propose` (MCP: `memory_propose`); the answer is `proposed`, `applied: false` — never "saved". An approval of kind `control` (options **Approve** / **Reject**; title `Memory proposal: …`) waits in the Memory page, under *Needs you* in Today, in `hutan approvals` and as a notification; the user answers **Approve** (applied exactly as proposed), **Edit & approve** (the user's text is applied instead) or **Reject** (nothing stored, never injected). Approval applies the reviewed payload against the `expected_revision` it was proposed for — there is no re-match: if the entry moved on the proposal ends in state `conflict`, visible, and nothing is applied. A proposal survives a daemon restart; at most 50 wait at once. Agents never write directly: `memory.write|update|archive|restore`, listing and answering proposals are user-only (`forbidden`), Mawas included.

**What a conversation is given.** `assistant.memory` (default on, live) enables it; off injects nothing, to any backend. At the start of a Mawas conversation the daemon renders a bounded, deterministic **snapshot** of the active global and current-workspace entries (Home's Mawas gets the global ones): profile, then fact, then note; newest revision first inside a category; each entry on one line with its id, revision and an *inferred* marker; at most **8 KiB**, and a cut is explicit — the text says how many entries are not listed and the manifest lists them in `omitted`. The block says the memory is background knowledge that cannot grant tools, widen a scope or override a rule, and how to propose a change. The exact text and a manifest are stored in `memory_snapshots`. An empty memory renders nothing (no block, no file); the empty snapshot is still recorded as the baseline for later deltas. Delivery per backend (§ Backends):

| Backend | How the snapshot is delivered |
|---|---|
| OMP | a daemon-owned private file `<data dir>/memory/<snapshot id>.md` (directory 0700, file 0600; Mawas's own prompt, then the memory) named as the value of the existing `--append-system-prompt` — memory text is never in argv. The file is written again, from the recorded snapshot, whenever the agent starts (a wake, a restart for a mode change, a daemon restart); files older than a week are removed at start |
| Codex | `developerInstructions` of `thread/start` and `thread/resume` (a typed field of Hutan's request; `baseInstructions` is left alone, so Codex keeps its own); a resumed thread gets the recorded snapshot again |
| Claude Code | a labelled block in the **first turn's** prompt content (an embedded resource, or quoted text when the adapter takes none). A system-prompt extension is not used: nothing in the handshake tells Hutan the adapter supports it, and the first-turn block is the portable route. A resumed conversation does not get it again |
| Provider (direct chat) | appended to the system message, fixed for that start |

**Deltas.** At each later turn boundary the daemon compares the scopes' revisions with the conversation's last snapshot or delta; when one moved it adds a compact labelled block to that turn — *Hutan memory update*: what was **added**, **changed** (with the revision the conversation knew) and **archived** ("stop using these"); an archive is never silently left out. A delta that would not fit 4 KiB is replaced by the complete bounded current view, which still names the archived entries. The block is Hutan-added context: it reaches an ACP agent as a resource apart from the user's words, Codex and provider chats as a delimited block after the message, the journal says so with a `notice` event ("Hutan memory update added to this turn's context: 1 added, 1 changed, 0 archived"), the desktop folds it like attached context, and it is recorded as a delta in `memory_snapshots` once the turn was accepted. Old transcript messages and a live system prefix are never rewritten. A conversation that began while memory was off is told the complete current view at its first turn after it is switched on.

**Deletion caveat.** Archiving stops an entry from being given to **future** conversations and deltas. It cannot recall bytes already sent to a provider or kept by a harness (its transcript, its system prompt of a running conversation), nor the local snapshots and journals of conversations that already got it. Memory is not a secret store: do not put a credential in it.

**Desktop.** The **Memory** page (`mawas-memory`, a registered panel shown as one tab of the center; ⌘K → *Mawas: Memory*, *Panel: Open Memory*, and *Open Memory* in the Today rail of Mawas Home; one instance) lists the entries grouped by category (Profile, Facts, Notes) with where each applies (*Global* or the workspace's name), its revision, an **inferred** badge and where it came from ("proposed by an agent in session …, approved by you in the desktop"); filters by scope (*All* — the global entries and those of the workspace the window shows —, *Global*, the workspace) and *Show archived*; **Add**, **Edit**, **Archive** / **Restore**, **Confirm** (an inferred entry) and **History** (the revisions below the row). Pending proposals are on top with **Approve**, **Edit & approve** and **Reject**; a pending proposal also appears in Today → *Needs you* as a card with the same three answers. Nothing is shown as saved before the daemon's receipt; a conflict or a refusal (a secret, a full memory) shows in the daemon's words. The page reads from the daemon when it is shown and when the daemon says the memory changed (`memory.changed`), with no timer; a hidden page does no work. Settings → Mawas → *Memory* has the two settings (`assistant.memory`, `assistant.memory_max_entries`). `assistant.enabled = false` removes the page and its commands with Mawas Home.

**CLI.** `hutan memory ls [--ws ID] [--category profile|fact|note] [--archived]`, `add <text> [--category …] [--ws ID]` (global `profile` by default; with `--ws` a `note` of that workspace), `edit <id> <text>`, `archive <id>`, `restore <id>`, `history <id>`, `proposals [--approve ID | --reject ID | --all]`; ids may be unique prefixes of at least 6 characters; every command takes `--json`; each write prints the daemon's receipt ("Saved: entry … revision 1 (receipt …, scope revision 4)"), and a proposal that ended in a conflict is a failed command (exit 1). Inside an agent's session the writes are refused.

**MCP.** `hutan mcp` has `memory_list` and `memory_search` (reads, free) and `memory_propose` (its result says it is **not saved** until the user approves it); there is no write tool.

## Idle assistants

A harness assistant (`omp`, `claude`, `codex`) is a helper for occasional questions, but its agent process is not small (an OMP backend holds about 460 MB), and every scope can have one. So an assistant that has not been used for a while gives the memory back and comes back on its own.

- **Sleep.** An assistant whose agent has been idle for `[assistant] idle_stop_secs` (default **600**; `0` never; otherwise at least 60; a live setting of `daemon.settings`, read at every check) has its agent process stopped by the daemon. Idle means: no turn, no pending approval, no pending question, and nothing has happened since the last turn end, approval answer or question answer (a turn start, an approval or a question being asked restarts the clock too). Only sessions marked `ChatInfo.assistant` are looked at: agent chats (`hutan chat new`, tasks) and direct or provider assistants are never stopped for being idle. The daemon logs `assistant <id> asleep, process stopped after N s idle` at info.
- **What clients see.** The session keeps its id, journal, model and state `idle`; it is not `stopped` or `failed`, there is no recovery chip and no `session.exited`. The only difference is that it has no process: `resources.list` shows none, and its agent tree is let go like that of any session whose agent ended.
- **Wake.** `chat.send` (and `chat.set_model` of a sleeping assistant, which needs the agent) starts the agent again, continuing the stored conversation the way a restart does (ACP `session/load`, else `session/resume`; Codex `thread/resume`), then delivers the message: the client sees the turn start a little later and no event a normal start would not emit. An agent that refuses to continue the conversation starts a new one next to the kept journal, and the journal gets the notice *The agent started a new conversation; the transcript is kept.* (the text of the recovery chip for the same case). A wake that fails altogether fails the send with the harness's error; the session stays `idle` and asleep, and the next send tries again. Two messages sent while it wakes share one start.
- **`assistant.open`** of a sleeping default returns it as it is: the panel shows the history at once, and the process starts with the first message. A stopped or failed default is restarted as before.
- **Stop and restart.** `session.stop` / `session.kill` of a sleeping assistant end it without starting anything. A daemon shutdown has nothing to stop in a sleeping assistant, and after a daemon restart *every* harness assistant comes back asleep: the daemon starts no agent for it at start (other chats are respawned as before; `restore_sessions = "stopped"` lists assistants as stopped like everything else). One whose last turn the shutdown cut off carries the usual `interrupted` recovery until the user writes to it.

## Attachments

`chat.send { session_id, text, attachments }`. At most 16 per message. Every chat takes attachments (protocol 0.23, `docs/protocol.md` § Rich input); `[assistant.context]` gates them for assistants and for agent-originated messages only. Every attachment is validated (its own bounds, then the permission of the session's scope) before the first one is read, and the first one that cannot be resolved fails the message: nothing is journaled, nothing reaches the provider.

| Kind | Fields | Read from | Cut keeps |
|---|---|---|---|
| `shell_block` | `terminal_id`, `block_id` | the terminal's output of that block (`terminal.block_text` path, M10) | the end |
| `shell_recent` | `terminal_id`, `lines` ≤ 500 | the last lines of the screen and scrollback (soft wraps joined; on a full-screen program only the visible screen) | the end |
| `agent_transcript` | `session_id`, `last_turns` ≤ 20 | a Hutan chat's journal (Codex, direct, assistant: the `message_end` events), else the history index rows of that session id (Claude Code, Codex, OMP; needs indexing) | the newest turns |
| `file` | `path`, `range?` `{start, end}` (1-based, inclusive) | the file, under the session's workspace roots only | the start |
| `search` | `query`, `limit` ≤ 10 | `history.search` (needs indexing): one line per session with its id, so that it can be attached next | the best hits |

Attachments also go to chats through the OMP and Claude Code harnesses (protocol 0.17, `docs/agents.md` § ACP harnesses): the agent gets each one as an ACP embedded resource, under the same permissions and bounds, and the journal's user message shows what it was given.

A turn is a user message and what follows it. A relative file path is relative to the first root of the workspace. Terminal and file text is plain text; a file that is not UTF-8 is `bad_request`.

**What the message becomes.** The user's text, then

```text
--- attached context (read-only, redacted, bounded by the user's settings) ---
[shell: cargo test -p daemon · exit 101 · 12 lines · cut to the last 3000 of 9000 characters]
…output…

[file: src/lib.rs · lines 1-40 · 40 lines]
…text…
--- end of attached context ---
```

Headers: `[shell: <command> · exit <code>|running · <n> lines]`, `[shell: terminal <id> · last <n> lines]`, `[agent: <title> · <hutan chat|claude|codex|omp> · <n> turns]`, `[file: <path> · lines a-b · <n> lines]`, `[search: "<query>" · <n> hits]`. A part that was cut says so (`cut to the first|last N of M characters`; M is left out when the source was only read in part, as for a big file), a source that was incomplete says `incomplete` (the terminal lost lines). This whole text is the journaled message: `chat.history` and the next turns' conversation contain it, so the conversation is reproducible and visible. The user's `message_end` event carries `payload.attachments_meta = [{kind, label, chars, truncated}]` so that a client can show chips without parsing text.

**Redaction.** Every text goes through the history index's redactor (`hutan_index::redact`: API keys, tokens, `Authorization:`/`Bearer`, PEM keys, `password=…` pairs, …; best effort, see `docs/history-index.md` § Privacy) *before* it is cut, so a secret straddling a cut is removed whole. Headers (a command line or a path may hold a secret) are redacted too. Files are read a little beyond their share (512 characters) so that redaction sees the end of a secret at the cut.

**Budget.** `assistant.context_budget_chars` (default 60 000) is split evenly over the attachments of one message (at least 1 character each); the user's own text and the earlier conversation are not part of it (the conversation is still limited by `gateway.max_context_chars`, oldest messages first; the new message, attachments included, is always sent).

**Files.** `context_file::read_file` canonicalises the path and the roots, requires the path to be inside a root (so `..`, an absolute path elsewhere, a symlink out and a sibling directory with the root's name as a prefix are refused with `forbidden`), and then opens it relative to a descriptor of the root with `openat(O_NOFOLLOW)` per component, the root's own ancestors included, so a symlink swapped in after the check fails the open instead of being followed. It reads only regular files, at most 8 MiB scanned, and builds the UTF-8 text from whole buffers (a character split by a read boundary is not an error).

## Permissions

```toml
# daemon.toml
[assistant]
provider = "openrouter"            # a provider of `hutan provider ls`; used for assistants created from now on
model = "meta-llama/llama-3.3-70b-instruct"
context_budget_chars = 60000
idle_stop_secs = 600               # a harness assistant idle this long stops its agent process; the next message wakes it. 0 = never, else at least 60
mode = "ask"                       # "ask" (Ask before acting, default) or "advisor" (read and advise only): § Modes and safeguards
max_actions_per_turn = 10          # changes Mawas may make in one turn before every further one asks (1-1000)
memory = true                     # give Mawas conversations the memory you approved (false: nothing is injected)
memory_max_entries = 200          # most active memory entries (1-10000)

[assistant.context]                # assistants of project workspaces
shell = true                       # shell_block, shell_recent
agents = true                      # agent_transcript
files = true                       # file
search = true                      # search

[assistant.global_context]         # the global (Home) assistant: files are off by default
files = false

[assistant.workspaces.hutan]      # per workspace, by name or id: only what differs from [assistant.context]
files = false
```

An attachment the settings do not allow is `forbidden`, and the message names the setting (`assistant.context.shell`, `assistant.global_context.files`, `assistant.workspaces.<key>.files`); the error's `data.setting` has it too. The permissions are read for each message, so a change made through `daemon.settings.set` (`hutan config set assistant.context.files false`, protocol 0.28, `docs/protocol.md` § Daemon settings) holds from the next message on; a hand edit of the file is picked up by the next such change or a restart. Settings gate the daemon's own reading; they do not make an attachment out of anything that was not attached.

Other refusals: `bad_request` (bounds, more than 16 attachments, a session that is not a chat), `not_found` (terminal, block, file, transcript), `invalid_state` (`search` or an external transcript while indexing is disabled).

## Indexing of chats

With `[history] enabled` the daemon feeds the journals of **all** chat sessions (Codex, direct, assistant) into the index as source `hutan`: user and assistant messages (`user`/`assistant` rows), tool calls and their output (`tool` rows), as they are journaled, keyed `c:<seq>`. The session id is the key and `history.search` hits carry it as `session_id` (also for terminal sessions), so a client can open the session; there is no `resume_argv` (resume = focus the session). Like all Hutan rows these are not backfilled from before indexing was on. A message with attachments is indexed with its attachments (redacted, at most 16 chunks).

## Desktop

**Mawas Home** (`assistant`, a registered panel like every other, shown as **one tab of the center**; it replaces the left-only page of OL-2) shows one Mawas chat with the ordinary chat transcript and composer of agent chats, about 46 rem wide and centered, and the collapsible **Today** rail on its right. The left nav's **Mawas** row (only while `assistant.enabled`), ⌘K → **Mawas: Open** / **Mawas: New chat**, a bound `OpenAssistant` key, the menu bar item and `assistant.global_shortcut` (e.g. `alt-space`, for the whole OS) open it; there is never more than one Home, it can be closed (× on the tab) and reopened, it is part of no preset, and it can still be moved like any panel. No key is bound by default. Opening it calls `assistant.open` and shows the returned session: no terminal is started.

**The header** is one row: the scope dropdown, the state dot, the **backend·model chip**, the **mode chip** (*Ask before acting ▾* / *Advisor ▾*: choosing one sets `assistant.mode`; the page says under the header that it applies from the next message, and the daemon restarts the agent for it), **New chat** (a fresh conversation in the shown scope, `assistant.open {new: true}`), **Stop** (the kill switch: denies every request the chat has pending, then interrupts its turn; enabled while a turn runs or a request waits) and the **Today** toggle. The empty conversation offers **Ask a question** (focuses the composer) and **Review what needs you** (jumps to the next session that needs the user), and says what is sent to the model.

**Today** (`panels/assistant_rail.rs`) is a view of the sessions store — the approvals, questions and agents every window already hears of — so it does no polling and no request of its own. *Needs you*: Mawas's approvals first (each card shows the request and the options the daemon offered, answered in place with `approval.respond`), then other agents' approvals, questions and blocked or failed sessions (**Open**); *Working*: the agents that run (a click opens the session); *Upcoming*: "No schedules yet". Memory, Skills, Schedules and Traces have no link until those services exist (ADR 0017 MW-2…MW-9). What Mawas asks of Hutan's own gate is also shown as cards above the conversation, because a harness's transcript cannot hold it.

**A hidden Home does no work**: the workbench tells it when it is the active tab of a pane (and when the user moved it to a dock, when that is visible); a hidden Home ignores store changes and draws again only when shown. There is no timer.

**Scope.** The header's scope dropdown chooses **Global** (the Home workspace's Mawas: it sees the Home folder only) or **Workspace · name** (the active workspace's Mawas; disabled while Home is active). Each scope has its own session, so switching keeps both transcripts, drafts and chips; nothing moves from one to the other. A history hit that is a Mawas conversation (`session_id` of a `hutan` hit) opens on Mawas Home, in its own scope, instead of as a tab; the Sessions panel still opens any Mawas chat as an ordinary chat tab, with the same composer.

**Backend and model.** The header is one compact row (scope dropdown, state dot, chip; `docs/design.md` § Layout). The chip shows what the shown scope's assistant runs on (`OMP · anthropic/claude-sonnet-4-5 ▾`, cut with "…" in a narrow sidebar; `Automatic` where the session's model is not known) and opens a menu: **Switch model…** and **Backends and models…**, the chooser: the backends (OMP, Claude Code, Codex with their detection and logins, and each provider), then the models of the one picked, or **Automatic** (no stored model: the daemon picks one, and the chooser says `Automatic · runs <model>` once the session exists). Choosing stores it with `assistant.configure` for the shown scope (Global for the Global tab, the workspace for the Workspace tab) and opens that scope's assistant on it.

**Where the defaults are set.** Settings → **Mawas** (desktop, ST-3, MW-1) sets, under *What Mawas may do*, the **mode** and **changes per turn** (`assistant.mode`, `assistant.max_actions_per_turn`, with `daemon.settings.set`); under *What you allowed Mawas to do without asking*, the **grants** the approvals stored — each with its tool, where it applies (a chat, or always in a workspace) and **Revoke**, and **Revoke all** (`control.grants.list|revoke`); the backend and model per scope (Global, and the workspace or project shown) with **Inherit** (`assistant.reset`), and the permissions of § Permissions (`assistant.context.*`, `assistant.global_context.*`, the overrides of the shown workspace with Inherit / Allow / Deny, `context_budget_chars`). The header's chooser still sets the shown scope. When nothing was chosen (no choice of this window, none in `daemon.toml`) the header says **`Automatic · <backend> · <model>`** and Settings says the same under the scope, so an automatic pick does not read like a pinned choice.

**Off.** `assistant.enabled = false` (settings namespace `assistant`; **default false** for a new settings file since the Orca-style layout, true for a user whose layout was saved before it, see `docs/design.md` § Layout) closes Mawas Home and hides its nav row, removes *Mawas: Open* / *Mawas: New chat* and *Panel: Open Mawas* from ⌘K, ignores an `OpenAssistant` binding and unregisters the system-wide shortcut; a history hit that is a Mawas conversation opens as an ordinary chat tab. The daemon keeps its sessions.

**Nothing ready.** With nothing configured the daemon's automatic backend opens Mawas by itself (§ Backends). When it answers `invalid_state` because nothing is ready, the page shows a card instead of the daemon's words: **Use OMP** when OMP is installed and logged in (it stores OMP globally and opens Mawas), **Add a provider…** (Models settings) and Retry. Nothing is sent anywhere until the user chooses. The first-run setup's Agents step preselects the first ready harness in the daemon's order (OMP, Claude Code, Codex); its **Mawas** switch turns `assistant.enabled` on.

**Without a provider (older daemons).** The daemon names the assistants' provider and model under `[assistant]` in `daemon.toml` and reads it at start. When `assistant.open` is refused with `invalid_state` the panel says so in the daemon's words, links to **Models settings** and offers Retry. The desktop never picks a provider or model of its own and makes no model call. A configured provider whose key is missing or rejected shows **Set key…** like any gateway chat, a missing model **Pick another model…**; the chip menu's **Switch model…** applies to assistants too.

**Context.** Context is opt-in per message and never read by the desktop. The composer's **＋ Attach** menu opens the form of each kind; typing a mention in the composer offers the same form (↹ or the hint's button accepts it and removes the mention from the draft):

| Mention | Form | Chip (`kind · source · bounds`) |
|---|---|---|
| `@shell` | the daemon's live terminals (those of the assistant's workspace first); **Recent output** (the field is the number of lines, default 50, at most 500) or **Blocks ▾** to pick one of the newest blocks | `shell recent · title · last 50 lines`, `shell block · title · $ cmd · block #7 · exit 1` |
| `@omp`, `@agent` | sessions that reported an agent session id, Hutan chats, and the indexed transcripts of the words typed after the mention (`@omp` is OMP only); a stepper for the turns (default 5, at most 20) | `agent · title · omp · last 5 turns` |
| `@file path[:first-last]` | the path and optional lines; with the argument typed the chip is made at once | `file · src/lib.rs · lines 1-40` |
| `@search words` | the words and a hits stepper (default 5, at most 10) | `search · “words” · 5 hits` |

Chips are removable (×) before the send and become structured attachments of `chat.send` (at most 16). An attachment that is not a chip is never read or added to the prompt. After the send the user's message shows what the daemon resolved for each attachment (`attachments_meta`: kind, label, characters kept, and a warning tint with "cut to the budget" when truncated), with the attached text folded away. A refusal (`forbidden`, `not_found`, `invalid_state`, bounds) shows the daemon's message in the banner, keeps the draft and the chips, marks the chips the settings refuse as *refused* and journals and sends nothing. File paths are checked by the daemon (see Files above): the form says so and never opens the file.

**Send to agent.** "Send to agent" of a finished terminal block (button, ⌘K, key) attaches it to the chat the user looked at last — Mawas counts once the user opened or focused Home after the last chat tab — or to the only running chat, or to the one picked from the menu. When that chat is Mawas the block becomes a `shell_block` attachment (a block reference the daemon reads when the message is sent; Home is shown and the chip waits in its composer); other chats keep the quoted-text chip. When several chats run and none is Mawas the menu adds **Send to Mawas**: the Mawas of the terminal's workspace, opened on demand. With no chat running the notice points at ⌘K → Mawas.

## CLI

```text
hutan ask "<question>" [--ws <id|name>] [--attach <spec>]…
hutan assistant ls [--ws <id|name>]
hutan assistant open [--ws <id|name>] [--new]
hutan assistant reset [--ws <id|name>]
hutan grants ls [--ws <id|name>]
hutan grants revoke <id> | --all
hutan memory ls|add|edit|archive|restore|history|proposals   (§ Memory)
```

`hutan grants ls|revoke` list and remove what Mawas may do without asking (user-only: refused inside an agent's session); an approval is answered with `hutan approve <id> allow_once|allow_chat|allow_workspace|deny` (`hutan approvals` lists the options). `--attach` specs: `block:<terminal>:<block>`, `recent:<terminal>:<lines>`, `agent:<session>[:<turns>]` (default 5), `file:<path>[:<first>-<last>]`, `search:<query>` (5 hits; quotes optional). `ask` uses the default Mawas of `--ws`, else `$HUTAN_WORKSPACE`, else the global one (Home), prints the answer as it streams and exits 1 when the turn fails (also when the daemon refuses an attachment: the message is on stderr). `--json` prints every event. `hutan chat log <session>` shows the whole conversation, attachments included.

## Tests

Resolver unit tests (`crates/daemon/src/{context,assistant,context_file}.rs`): bounds, cuts and headers, redaction before the cut, row joining, turn counting, path confinement including `..`, absolute paths, symlinks to files and directories, symlinked ancestors of a root and prefix siblings. The model pick (`crates/daemon/src/assistant_pick.rs`) over realistic catalogs: OMP (several providers, versions, dates, never across providers), Claude Code's ACP adapter, Codex (`-pro`), Google, no lower tier, empty and hidden. Daemon tests (`crates/daemon/tests/assistant.rs`) run a real daemon with the in-memory credential store, a mock OpenAI-compatible server (`hutan_gateway::testing::MockServer`), real terminals with OSC 133 blocks and the history index on temporary directories: `assistant.open`/`list` per scope, restart and stop; every attachment kind resolved, journaled (with `attachments_meta`) and sent, the provider's request asserted (headers present, unattached terminal/file text absent, secrets redacted); refusals by setting and by confinement, bounds, nothing sent; the budget split and the cut headers; external transcripts from the index; chat journals found by `history.search` with `session_id`. `crates/daemon/tests/assistant_backend.rs` runs fake harnesses: the automatic backend (a ready OMP and the model one tier below its default, nothing ready naming every candidate, a keyless provider not ready and a local one ready) and a stored model used as it is. CLI tests (`crates/cli/tests/assistant.rs`) drive the real `hutan` binary with every attachment kind.

Idle assistants (`crates/daemon/tests/assistant_sleep.rs`, a real daemon with the fake ACP agent and the idle time shortened through `DaemonConfig::assistant_idle_stop`): the agent process exits while `session.list` still shows the session `idle` with its history, model and no recovery; a send starts a new process that continues with `session/load` of the old session id, the turn streams and the journal is continuous; a pending approval keeps the process (and answering it restarts the clock); `assistant.open` and `chat.history` of a sleeping assistant start nothing; a daemon restart (from asleep and from awake) restores the assistant without a process and the first send starts it; agent chats are untouched; a refused `session/load` starts a new conversation and journals the notice; a failed wake fails the send and a retry works; a sleeping assistant can be stopped without starting it. The setting's parsing (default, `0`, minimum 60) is unit-tested in `config.rs`.

Desktop tests (`crates/desktop/tests/assistant_gpui.rs`, GPUI against an in-process daemon with the in-memory credential store and the same mock server): the global and a workspace assistant stream with no terminal and each scope keeps its transcript, draft and chips; every chip kind added through the mentions and the Attach menu, removed, and only the selected sources in the provider's prompt; truncation metadata after the send; refusals by setting and by confinement (chips marked, draft restored, nothing journaled or sent); a relative path no desktop process can resolve read by the daemon; a history hit of an assistant conversation opened in the panel and continued; the unconfigured prerequisite with no model call; a terminal block sent to the assistant as a `shell_block` attachment.

Memory (MW-2): storage (`crates/storage/src/tests.rs`: revisions immutable even for a direct statement, `expected_revision` conflict, repeated `operation_id` returns the first receipt, archive and restore keep history, the bound, snapshot order, a proposal applied, rejected or left as a conflict in one transaction); daemon (`crates/daemon/tests/memory.rs`, a real daemon, the fake ACP and Codex agents and a mock provider: agent-origin writes, updates and answers are `forbidden`, a proposal applies exactly what was reviewed and its provenance is the daemon's, a conflict stays visible, secrets are refused, bounds, snapshot determinism and the explicit cut, the OMP prompt file (0600, never argv), Codex `developerInstructions` on start and resume, Claude's first-turn block, the provider's system message, a delta at the next turn, the recorded snapshot on resume, `assistant.memory = false` injecting nothing); CLI (`crates/cli/tests/memory.rs`, `mcp.rs`); desktop (`crates/desktop/tests/memory_gpui.rs`: the page lists, adds, edits, archives, confirms and shows history, proposals are answered from the page and from Today, a hidden page does no work, the settings).

Mawas's safeguards (`crates/daemon/tests/mawas.rs`, a real daemon with `[control] agents = "allow"`, real client connections that say they run inside a Mawas chat, the fake ACP agent for the harness): a change asks despite the policy while another agent's runs; Allow for this chat covers that chat and that tool only; Always allow here covers the workspace's chats and not another workspace; a destructive request asks despite a grant in the database and offers no grant; an agent-originated connection (Mawas or any other) cannot list, revoke or answer grants and approvals; Advisor refuses with its reason, reads work, and the mode is live; the turn limit forces a prompt that no grant lifts; the log and the table record every decision; grants survive a daemon restart and revoking makes the request ask again; a change of mode restarts the agent before the next turn and the journal says so. Launch flags per mode (`crates/daemon/src/harness.rs` unit tests on the built ACP config and the profiles; `crates/daemon/tests/assistant_backend.rs` against the fake OMP, Claude Code and Codex): OMP's `--tools`/`--approval-mode`, Claude Code's `plan`/`default`, Codex's sandbox and approval policy. CLI: `crates/cli/tests/grants.rs`. Desktop (`crates/desktop/tests/mawas_gpui.rs`, `layout_gpui.rs`): the nav row opens one Home in the center, the mode chip switches and shows the next-turn note, Today lists Mawas's approval first and answering it there runs the request, Stop denies and interrupts, New chat, a hidden Home is not drawn when the store changes, Settings → Mawas lists and revokes grants, `assistant.enabled = false` hides the row.

## Limits

Attachments are text only; a file larger than the scan bound (8 MiB) is read from its start. `shell_recent` joins soft-wrapped rows but cannot tell a hard line break from a wrap on a program that draws its own screen. An external transcript is read from the index (redacted chunks of at most 16 per message, merged back into messages), not from the transcript file. Settings are read at daemon start. In Advisor mode Mawas cannot run commands or change files; in Ask before acting it can, only with the user's approval, and the daemon's gate rests on the origin of the request, which is cooperative (`docs/control.md` § Mawas): a restricted endpoint with a sandbox for unattended runs is ADR 0017's MW-10. A direct-provider Mawas is *Chat only*: it has no tools. The desktop cannot read the daemon's `[assistant]` settings: it learns that a provider is missing from `assistant.open`'s refusal and that a context kind is not allowed from `forbidden` on the first message that uses it, not before.
