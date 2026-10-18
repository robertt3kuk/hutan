# Assistant chat (roadmap M16: the daemon side, and the desktop panel in § Desktop)

Decisions: ADR 0008 (no secrets in context; credentials), ADR 0001 (the daemon owns conversations). Wire messages: `docs/protocol.md` § Assistant chat and context attachments. The direct chat it is built on: `docs/gateway.md`. The index it reads and feeds: `docs/history-index.md`. The desktop panel for it is described in § Desktop.

An **assistant** is a direct chat (a provider's chat API, no tools, no approvals) that lives in a scope, starts from a system prompt that explains attachments, and can be sent **context**: shell output, recent terminal lines, the conversation of a coding agent, a file, search results. The context is chosen by the user for each message, read by the daemon, redacted, bounded and journaled with the message. Nothing is read that was not attached.

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

What each harness assistant may do (restricted where the harness allows it, never faked):

| Backend | How it runs | Tools |
|---|---|---|
| `omp` | `omp acp --tools read,grep,glob --append-system-prompt <assistant prompt>` | read-only: no `bash`, `edit`, `write`, `task`, web |
| `claude` | Claude Code's ACP session in its `plan` mode | read-only planning mode of Claude Code; what it lists as tools in that mode is Claude Code's own |
| `codex` | `codex app-server` with `sandbox: read-only`, approval policy `never` | read-only sandbox, nothing asks (a refused write fails inside the sandbox) |
| `provider` | the direct chat gateway | none |

The permissions below (`[assistant.context]` …) gate attachments for every backend. A harness assistant's session is marked `ChatInfo.assistant`; it is an ordinary harness chat otherwise (journal, restore, history index). Harness assistants need the harness's login (`hutan account ls`); when the harness is missing or logged out `assistant.open` fails with the harness's own message (when it was chosen; the automatic backend only picks a ready harness).

**Resetting a scope.** `assistant.configure` stores a choice per scope and there is no such thing as "automatic" among its backends, so a choice made once would stay. `assistant.reset {scope, workspace_id?}` (protocol 0.28; `hutan assistant reset [--ws ID]`) removes the stored choice of that scope (the `app_meta` key it was written to): the scope inherits again, workspace → its parent workspace → the global choice → `[assistant]` of `daemon.toml` → the automatic backend. Assistant sessions that exist are untouched and `assistant.open` follows the inherited choice from then on (the oldest assistant that runs on it is the default, one is created when there is none). Resetting a scope with nothing stored is not an error. Like `daemon.settings.set` it is user-only: an agent-originated connection gets `forbidden` under every `[control]` policy. The `[assistant]` defaults (`backend`, `provider`, `model`, `context_budget_chars`, `idle_stop_secs`) and the permissions are also settings of `daemon.settings`, live.

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

The **Assistant** panel (`assistant`, registered like every panel but placed in the **left dock only**: it is the *Assistant* page of the left sidebar's nav, shown in place of the project list, never a right-dock or center item and never part of a layout preset; listed under ⌘K → **Panel: Open Assistant**; only while `assistant.enabled`) shows one assistant chat with the ordinary chat transcript and composer of agent chats. ⌘K → **Assistant: Open / Focus** reveals it and puts the keyboard in its composer. No key is bound by default: bind `OpenAssistant` in `keymap.toml` and/or set `assistant.global_shortcut` (e.g. `alt-space`) for the whole OS. Opening an assistant calls `assistant.open` and shows the returned direct-chat session: no terminal and no vendor agent process is started.

**Scope.** The header's scope dropdown chooses **Global** (the Home assistant: it sees the Home folder only) or **Workspace · name** (the active workspace's assistant; disabled while Home is active). Each scope has its own session, so switching keeps both transcripts, drafts and chips; nothing moves from one to the other. A history hit that is an assistant conversation (`session_id` of a `hutan` hit) opens in the panel, in its own scope, instead of as a tab; the Sessions panel still opens any assistant as an ordinary chat tab, with the same composer.

**Backend and model.** The header is one compact row (scope dropdown, state dot, chip; `docs/design.md` § Layout). The chip shows what the shown scope's assistant runs on (`OMP · anthropic/claude-sonnet-4-5 ▾`, cut with "…" in a narrow sidebar; `Automatic` where the session's model is not known) and opens a menu: **Switch model…** and **Backends and models…**, the chooser: the backends (OMP, Claude Code, Codex with their detection and logins, and each provider), then the models of the one picked, or **Automatic** (no stored model: the daemon picks one, and the chooser says `Automatic · runs <model>` once the session exists). Choosing stores it with `assistant.configure` for the shown scope (Global for the Global tab, the workspace for the Workspace tab) and opens that scope's assistant on it.

**Where the defaults are set.** Settings → **Assistant** (desktop, ST-3) sets the backend and model per scope (Global, and the workspace or project shown) with **Inherit** (`assistant.reset`), and the permissions of this section (`assistant.context.*`, `assistant.global_context.*`, the overrides of the shown workspace with Inherit / Allow / Deny, `context_budget_chars`) with `daemon.settings.set`, which writes `daemon.toml`. The header's chooser still sets the shown scope. When nothing was chosen (no choice of this window, none in `daemon.toml`) the header says **`Automatic · <backend> · <model>`** and Settings says the same under the scope, so an automatic pick does not read like a pinned choice. The daemon does not tell which scopes have a stored choice, so a choice made by another client or before the window started is not known to this label.

**Off.** `assistant.enabled = false` (settings namespace `assistant`; **default false** for a new settings file since the Orca-style layout, true for a user whose layout was saved before it, see `docs/design.md` § Layout) closes and hides the Assistant panel and its nav row, removes *Assistant: Open / Focus* and *Panel: Open Assistant* from ⌘K, ignores an `OpenAssistant` binding and unregisters the system-wide shortcut; a history hit that is an assistant conversation opens as an ordinary chat tab. The daemon keeps its assistant sessions.

**Nothing ready.** With nothing configured the daemon's automatic backend opens the assistant by itself (§ Backends). When it answers `invalid_state` because nothing is ready, the panel shows a card instead of the daemon's words: **Use OMP** when OMP is installed and logged in (it stores OMP globally and opens the assistant), **Add a provider…** (Models settings) and Retry. Nothing is sent anywhere until the user chooses. The first-run setup's Agents step preselects the first ready harness in the daemon's order (OMP, Claude Code, Codex).

**Without a provider (older daemons).** The daemon names the assistants' provider and model under `[assistant]` in `daemon.toml` and reads it at start. When `assistant.open` is refused with `invalid_state` the panel says so in the daemon's words, links to **Models settings** and offers Retry. The desktop never picks a provider or model of its own and makes no model call. A configured provider whose key is missing or rejected shows **Set key…** like any gateway chat, a missing model **Pick another model…**; the chip menu's **Switch model…** applies to assistants too.

**Context.** Context is opt-in per message and never read by the desktop. The composer's **＋ Attach** menu opens the form of each kind; typing a mention in the composer offers the same form (↹ or the hint's button accepts it and removes the mention from the draft):

| Mention | Form | Chip (`kind · source · bounds`) |
|---|---|---|
| `@shell` | the daemon's live terminals (those of the assistant's workspace first); **Recent output** (the field is the number of lines, default 50, at most 500) or **Blocks ▾** to pick one of the newest blocks | `shell recent · title · last 50 lines`, `shell block · title · $ cmd · block #7 · exit 1` |
| `@omp`, `@agent` | sessions that reported an agent session id, Hutan chats, and the indexed transcripts of the words typed after the mention (`@omp` is OMP only); a stepper for the turns (default 5, at most 20) | `agent · title · omp · last 5 turns` |
| `@file path[:first-last]` | the path and optional lines; with the argument typed the chip is made at once | `file · src/lib.rs · lines 1-40` |
| `@search words` | the words and a hits stepper (default 5, at most 10) | `search · “words” · 5 hits` |

Chips are removable (×) before the send and become structured attachments of `chat.send` (at most 16). An attachment that is not a chip is never read or added to the prompt. After the send the user's message shows what the daemon resolved for each attachment (`attachments_meta`: kind, label, characters kept, and a warning tint with "cut to the budget" when truncated), with the attached text folded away. A refusal (`forbidden`, `not_found`, `invalid_state`, bounds) shows the daemon's message in the banner, keeps the draft and the chips, marks the chips the settings refuse as *refused* and journals and sends nothing. File paths are checked by the daemon (see Files above): the form says so and never opens the file.

**Send to agent.** "Send to agent" of a finished terminal block (button, ⌘K, key) attaches it to the chat the user looked at last — the assistant the Assistant panel shows counts once the user opened or focused it after the last chat tab — or to the only running chat, or to the one picked from the menu. When that chat is an assistant the block becomes a `shell_block` attachment (a block reference the daemon reads when the message is sent; the panel is shown and the chip waits in its composer); other chats keep the quoted-text chip. When several chats run and none is an assistant the menu adds **Send to Assistant**: the assistant of the terminal's workspace, opened on demand. With no chat running the notice points at ⌘K → Assistant.

## CLI

```text
hutan ask "<question>" [--ws <id|name>] [--attach <spec>]…
hutan assistant ls [--ws <id|name>]
hutan assistant open [--ws <id|name>] [--new]
hutan assistant reset [--ws <id|name>]
```

`--attach` specs: `block:<terminal>:<block>`, `recent:<terminal>:<lines>`, `agent:<session>[:<turns>]` (default 5), `file:<path>[:<first>-<last>]`, `search:<query>` (5 hits; quotes optional). `ask` uses the default assistant of `--ws`, else `$HUTAN_WORKSPACE`, else the global one (Home), prints the answer as it streams and exits 1 when the turn fails (also when the daemon refuses an attachment: the message is on stderr). `--json` prints every event. `hutan chat log <session>` shows the whole conversation, attachments included.

## Tests

Resolver unit tests (`crates/daemon/src/{context,assistant,context_file}.rs`): bounds, cuts and headers, redaction before the cut, row joining, turn counting, path confinement including `..`, absolute paths, symlinks to files and directories, symlinked ancestors of a root and prefix siblings. The model pick (`crates/daemon/src/assistant_pick.rs`) over realistic catalogs: OMP (several providers, versions, dates, never across providers), Claude Code's ACP adapter, Codex (`-pro`), Google, no lower tier, empty and hidden. Daemon tests (`crates/daemon/tests/assistant.rs`) run a real daemon with the in-memory credential store, a mock OpenAI-compatible server (`hutan_gateway::testing::MockServer`), real terminals with OSC 133 blocks and the history index on temporary directories: `assistant.open`/`list` per scope, restart and stop; every attachment kind resolved, journaled (with `attachments_meta`) and sent, the provider's request asserted (headers present, unattached terminal/file text absent, secrets redacted); refusals by setting and by confinement, bounds, nothing sent; the budget split and the cut headers; external transcripts from the index; chat journals found by `history.search` with `session_id`. `crates/daemon/tests/assistant_backend.rs` runs fake harnesses: the automatic backend (a ready OMP and the model one tier below its default, nothing ready naming every candidate, a keyless provider not ready and a local one ready) and a stored model used as it is. CLI tests (`crates/cli/tests/assistant.rs`) drive the real `hutan` binary with every attachment kind.

Idle assistants (`crates/daemon/tests/assistant_sleep.rs`, a real daemon with the fake ACP agent and the idle time shortened through `DaemonConfig::assistant_idle_stop`): the agent process exits while `session.list` still shows the session `idle` with its history, model and no recovery; a send starts a new process that continues with `session/load` of the old session id, the turn streams and the journal is continuous; a pending approval keeps the process (and answering it restarts the clock); `assistant.open` and `chat.history` of a sleeping assistant start nothing; a daemon restart (from asleep and from awake) restores the assistant without a process and the first send starts it; agent chats are untouched; a refused `session/load` starts a new conversation and journals the notice; a failed wake fails the send and a retry works; a sleeping assistant can be stopped without starting it. The setting's parsing (default, `0`, minimum 60) is unit-tested in `config.rs`.

Desktop tests (`crates/desktop/tests/assistant_gpui.rs`, GPUI against an in-process daemon with the in-memory credential store and the same mock server): the global and a workspace assistant stream with no terminal and each scope keeps its transcript, draft and chips; every chip kind added through the mentions and the Attach menu, removed, and only the selected sources in the provider's prompt; truncation metadata after the send; refusals by setting and by confinement (chips marked, draft restored, nothing journaled or sent); a relative path no desktop process can resolve read by the daemon; a history hit of an assistant conversation opened in the panel and continued; the unconfigured prerequisite with no model call; a terminal block sent to the assistant as a `shell_block` attachment.

## Limits

Attachments are text only; a file larger than the scan bound (8 MiB) is read from its start. `shell_recent` joins soft-wrapped rows but cannot tell a hard line break from a wrap on a program that draws its own screen. An external transcript is read from the index (redacted chunks of at most 16 per message, merged back into messages), not from the transcript file. Settings are read at daemon start. The assistant cannot run commands or change files. The desktop cannot read the daemon's `[assistant]` settings: it learns that a provider is missing from `assistant.open`'s refusal and that a context kind is not allowed from `forbidden` on the first message that uses it, not before.
