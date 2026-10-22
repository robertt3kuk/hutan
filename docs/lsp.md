# Language servers (E-11b, protocol 0.39, capabilities `doc.sync`, `lsp`)

The daemon runs language servers for the editor and for agents: one server per (server binary, root), shared by every client. It finds servers on the login `PATH`, never downloads one, never starts one in a folder the user has not trusted, and shows each server's process tree in Resources. This page is the daemon, the protocol, `hutan lsp` / `hutan diagnostics` and the MCP tools (E-11b); the editor's side — diagnostics, hover, go to definition, the trust bar and the Problems panel (E-12a) — is `docs/editor.md` § Language servers in file tabs and `docs/design.md` § File tabs and § Problems panel.

Wire: `docs/protocol.md` § LSP. Code: `crates/daemon/src/lsp/` (`hub.rs` the `LspHub`, `catalog.rs` servers and languages, `roots.rs`, `trust.rs`, `convert.rs`), the client library `crates/lsp_client` (E-11a, `docs/editor.md` § LSP client), `crates/protocol/src/lsp.rs`, `crates/client/src/lsp.rs` (the `Client` calls) and `crates/client/src/connection.rs` (the same on the desktop's `Connection`, with the `lsp.state` / `lsp.diagnostics` events as notices), `crates/cli/src/commands/lsp.rs` and `crates/cli/src/diag.rs`. Decisions: ADR 0012 amendment 2026-10-21 points 3–5; evidence: `docs/spikes/2026-10-22-editor-research/lsp.md`.

## Servers: found, never fetched

A server is a row of the built-in table, optionally changed by `daemon.toml [lsp.servers.<id>]`, or a server of your own. The command is looked up like the harnesses' (`docs/agents.md`): the login environment's `PATH` first (the one a new terminal gets, B-1), then the daemon's own, then the folders installers use (`~/.cargo/bin`, `~/.bun/bin`, `~/.local/bin`, Homebrew, npm and pnpm prefixes, nvm/fnm/volta shims…). The first installed server of a language serves it.

| id | command | languages | root markers | install hint (shown, never run) |
|---|---|---|---|---|
| `rust-analyzer` | `rust-analyzer` | `rust` | `Cargo.toml`, `rust-project.json` | `rustup component add rust-analyzer` |
| `typescript-language-server` | `typescript-language-server --stdio` | `typescript`, `typescriptreact`, `javascript`, `javascriptreact` | `tsconfig.json`, `jsconfig.json`, `package.json` | `npm install -g typescript-language-server typescript` |
| `pyright` | `pyright-langserver --stdio` | `python` | `pyrightconfig.json`, `pyproject.toml`, `setup.py`, `setup.cfg`, `requirements.txt` | `npm install -g pyright` |
| `basedpyright` | `basedpyright-langserver --stdio` | `python` | as `pyright` | `pip install basedpyright` |
| `gopls` | `gopls` | `go`, `gomod` | `go.work`, `go.mod` | `go install golang.org/x/tools/gopls@latest` |
| `clangd` | `clangd` | `c`, `cpp`, `objective-c`, `objective-cpp` | `compile_commands.json`, `compile_flags.txt`, `.clangd` | `brew install llvm` |
| `bash-language-server` | `bash-language-server start` | `shellscript` | — | `npm install -g bash-language-server` |
| `vscode-json-languageserver` | `vscode-json-language-server --stdio` | `json`, `jsonc` | — | `npm install -g vscode-langservers-extracted` |
| `taplo` | `taplo lsp stdio` | `toml` | — | `cargo install taplo-cli --locked --features lsp` |
| `marksman` | `marksman server` | `markdown` | `.marksman.toml` | `brew install marksman` |

The language of a file comes from its name (`.rs` → `rust`, `.tsx` → `typescriptreact`, `.sh` → `shellscript`, …); an extension with no entry names itself (`.zig` → `zig`), which is what a server of your own lists. `doc.open` may name the language.

**Missing.** A server whose binary is not found is the state `not_installed`: `reason` says what was looked for, `install` is the exact command that installs it, `setting` is `lsp.servers.<id>.command`, the key of `daemon.toml` that points at a binary somewhere else. `hutan lsp restart <id>` (or the next document) looks again, so installing and retrying needs no daemon restart. Nothing is downloaded or installed by Hutan.

## Configuration (`daemon.toml`, read when the daemon starts)

```toml
[lsp]
enabled = true            # false: no document is sent to any server
trust = "ask"             # "ask" (default), "always", "never": see Trust
idle_stop_secs = 300      # a server with no open document stops after this long; 0 = never
max_servers = 4           # servers running at once (1–32)

[lsp.servers.rust-analyzer]   # a built-in server: only what differs
command = "/Users/me/.cargo/bin/rust-analyzer"
args = []
env = { RA_LOG = "warn" }                     # added to the login environment
languages = ["rust"]
root_markers = ["Cargo.toml", "rust-project.json"]
initialization_options = { cargo = { allTargets = false } }
settings = {}                                  # what workspace/configuration answers
idle_stop_secs = 600                           # this server only
install = "rustup component add rust-analyzer"
max_failures = 5                               # crashes within failure_window_secs make it `failed`
failure_window_secs = 180
max_documents = 4096

[lsp.servers.gopls]
enabled = false                                # never use it

[lsp.servers.zls]                              # a server of your own: command and languages are required
command = "zls"
languages = ["zig"]
root_markers = ["build.zig"]

[lsp.projects.hutan]                           # per project (name or id): only what differs
enabled = false                                # no server starts for this project's files
[lsp.projects.hutan.roots]
rust-analyzer = "crates/core"                  # this server's root for the project's files (relative to the project's folder, or absolute)
```

A server of your own comes before the built-in ones for its languages. A typo is an error at start (`deny_unknown_fields`), like the other tables. The list in Settings does not show `[lsp]`: it has tables of its own; `daemon.toml` is where it is set.

## One server per (server, root)

- **Root.** The topmost folder with one of the server's root markers above the file, within the project: the folder of the workspace the file belongs to or the git checkout that contains it, whichever is deeper. The nearest marker would split a monorepo into one server per package (nvim-lspconfig #3818, #4457, #4227); the topmost lets the server see the workspace (a Cargo workspace, a `tsconfig` with references). Without a marker the root is the project folder; a file in no project and no checkout gets the nearest marker. `[lsp.projects.<p>.roots]` overrides it for one server and project.
- **Shared.** A document uses an existing server of the same binary when its root is inside that server's root, whoever opened it. The desktop, `hutan diagnostics` and an agent's connection share one process; `lsp.status` shows one server and how many documents it has.
- **Start on demand.** The first `doc.open` of a language in a trusted folder starts the server; the documents already open are sent to it. The server announces itself with `lsp.state` events.
- **State.** `starting`, `indexing` (work-done progress runs), `ready`, `failed`, `stopped`, plus `not_installed` and `untrusted` when there is no process. A crash restarts with a growing backoff (1 s doubling to 30 s) and open documents are sent again; 5 crashes within 3 minutes make it `failed` (`hutan lsp restart` tries again).
- **Idle stop.** A running server whose last document closed stops after `idle_stop_secs`; the next document starts it again.
- **`max_servers`.** Starting a server when that many run stops the least recently used one that has no open documents (`reason`: "stopped to make room"); when every running server has documents the new one stays `stopped` with that reason, and `hutan lsp stop` or closing documents makes room.
- **Processes.** A server runs in its own process group, with the login environment plus its `env`, in its root. Its tree (rust-analyzer's proc-macro server, cargo) shows in `hutan ps` and the Resources panel as a `service` node with RSS and CPU; `lsp.status` carries the tree's RSS. Servers and their children are killed when the daemon ends. They are separate processes: they are not part of `hutand`'s own memory budget, they are bounded by `max_servers` and idle stop.

## Trust

A language server runs the project's code: rust-analyzer executes build scripts and procedural macros, and servers execute commands named in workspace settings. **rust-analyzer does not sandbox any of it.** Hutan therefore asks once per folder before a server starts there, and the decision covers build scripts, proc macros and executable paths in workspace settings (one decision, not three), for the folder and everything below it that has no decision of its own (the deepest folder wins).

- `[lsp] trust = "ask"` (default): a document in an undecided folder gets a server in the state `untrusted` with `trust_pending: true` — the request, as an `lsp.state` event and in `lsp.status`, which the desktop and the CLI show. No process exists until the user answers. `"always"` trusts every folder, `"never"` none; a decision of the user wins over either.
- **Answering.** `hutan lsp trust FOLDER` (default: the current directory; `--deny`; `--forget` to ask again) sends `lsp.trust {folder, decision}`. Trusting starts the servers of the documents that wait; denying or forgetting stops the servers running there. Decisions are stored in the database (`lsp_trust`), survive a restart and are listed by `hutan lsp status`.
- **User-only.** `lsp.trust` is `Access::UserOnly`: an agent-originated connection (a chat session, or a shell where an agent reported itself) gets `forbidden` under every `[control]` policy, and an agent's `lsp.restart` cannot start a server in an untrusted folder. The request to trust is raised by the daemon, not by the agent.
- A request nobody waits on any more (the last document of an untrusted folder closed) disappears.

## Documents

`doc.open` hands the daemon the text of a file a client has open; the daemon mirrors it (≤ 3 MiB per `doc.open`, ≤ 16 MiB once edits grew it, ≤ 128 documents per connection, 256 MiB in all) and feeds the server. `doc.change` carries LSP-shaped edits with UTF-16 positions (or the whole text); a burst of edits is coalesced to one `didChange` per 50 ms window and flushed before any request (E-11a). A document is owned by the first connection that opens it; a second connection follows it (`owner: false`) and sees its diagnostics. A connection's documents close when it goes away. Positions on the wire are UTF-16 whatever the server negotiated (UTF-8 is preferred and converted).

An agent may open documents only below the workspace folders (`forbidden` otherwise); the user's connections may open any file.

## Diagnostics

- Servers publish; the daemon keeps the latest set per file (≤ 500 per file, 4096 files per server, messages ≤ 4 KiB) and sends `lsp.diagnostics` events — the whole set, replacing the file's — to the connections that have the file open, at most 4 a second per file. `lsp.diagnostics {path | workspace_id | none}` reads them for anyone; `stale` says some server was not `ready` (starting, indexing, not running): the answer may be incomplete, and a server that is not installed, not trusted or failed is never "no problems".
- **`hutan diagnostics [PATH] [--severity LEVEL] [--wait SECS] [--json]`.** For a file: no desktop needed — the CLI reads the file through the daemon, opens it as a document, waits for the server (up to `--wait`, default 30 s: until it is ready and has published for the file, or has been ready and quiet for 1.5 s), prints `path:line:col: severity: message [source code]` (one-based) with a summary, and closes the document so the server can go idle. For a folder: what the servers know below it. A missing, untrusted or failed server is explained with its install command or the `hutan lsp trust` to run.
- **MCP `diagnostics`.** Read-only. By default (`scope: "changed"`) the files `git status` reports in the agent's checkout (modified, added, untracked) **plus the files that directly import them** (a text search through `fs.search`: `mod`/`use` for Rust, `import`/`require` for TypeScript and JavaScript, `import`/`from` for Python, the package path for Go, `#include` for C and C++; at most 40 files, 30 importers), because that is where an edit breaks things (OpenCode #16353). `paths` checks exactly those files; `scope: "project"` returns everything the servers know about the session's workspace and only when asked. Results are `stale` until the servers are ready. LSP is deliberately not offered as a references or grep replacement (`docs/spikes/2026-10-22-editor-research/lsp.md` §6: grep beat it nine times out of nine); `lsp_status` lists the servers.

## CLI

```text
hutan lsp [ls]                  servers: state, name, documents, memory, root, and what to do about a missing or untrusted one
hutan lsp status                ls with ids, pids, capabilities, the trust default, max servers and every folder decision
hutan lsp restart SERVER        start again (a missing binary is looked for again); SERVER is the id or a unique name
hutan lsp stop SERVER           stop; the next document that needs it starts it again
hutan lsp trust [FOLDER] [--deny | --forget]
hutan diagnostics [PATH] [--severity error|warning|info|hint] [--wait SECS] [--json]
```

`hutan ps` shows the servers as `lsp <name> <root>` blocks after the sessions. All of it is also the protocol (`doc.*`, `lsp.*`) and, for agents, MCP (`diagnostics`, `lsp_status`): `hutan api lsp.status '{}'`.

## Limits

No unbounded channel anywhere (a test greps `crates/daemon/src/lsp` and `crates/lsp_client`). Per server: E-11a's bounds (frames, queues, pending requests, diagnostics store, log ring). In the hub: 64 slots (server, root), the mirrors above, 512 files read from disk to convert one answer's UTF-8 columns (beyond, a UTF-8 server's columns of non-ASCII lines are bytes), 100 locations per `lsp.definition`, 64 KiB per hover. A diagnostics event to a client whose queue is full is dropped (never closes the connection); `lsp.diagnostics` reads the truth.

## Troubleshooting

| Symptom | Why / what to do |
|---|---|
| `not_installed` | The binary is not on the login `PATH`. Run the `install` command shown (Hutan never does), or set `[lsp.servers.<id>] command` in `daemon.toml` to its path; `hutan lsp restart <id>` looks again. A tool installed in a place only an interactive shell adds to `PATH` is found in the usual installer folders; if yours is elsewhere, set `command`. |
| `untrusted`, `trust_pending` | The folder was never decided: `hutan lsp trust FOLDER` if you would run that project's build scripts. A denied folder (`trust_pending` absent) stays closed until `--forget` or `trust`. |
| `failed` | It crashed 5 times in 3 minutes; `reason` has the last exit and stderr line. Fix the cause (toolchain, config), then `hutan lsp restart`. |
| `stopped`, "idle" | It stopped with no open document; the next one starts it. "stopped to make room" / "`max_servers`": close documents or raise `max_servers`. |
| Diagnostics are `stale` | The server is still indexing (rust-analyzer on a large workspace takes a while); ask again when `hutan lsp ls` says `ready`. |
| A second server for one repository | Two roots: a nested project with its own marker outside the topmost one's folder, or `[lsp.projects]` roots. `hutan lsp status` shows each root. |
| Nothing happens for a file type | `hutan lsp status` shows no server for the language; configure one under `[lsp.servers.<id>]` with `languages` naming the file's language id (its extension for uncommon ones). |
| rust-analyzer uses much memory | It is a separate process (see `hutan ps`); lower `max_servers`/`idle_stop_secs`, or `hutan lsp stop` it. |

## Tests

`crates/daemon/tests/it/lsp.rs` (a real daemon, real connections and `hutan-fake-lsp`): initialize, diagnostics (UTF-8 server columns become UTF-16), hover and definition; a crash restarts with backoff then `failed`; idle stop and restart on the next open, with the process gone (by pid); an untrusted folder never spawns (no process on the table), an agent's `lsp.trust` is `forbidden`, trust starts it, denying stops it, the decision survives a daemon restart; a burst of 100 `doc.change`s is one `didChange` with the final text; edits in UTF-16 in order and all-or-nothing; two connections (a desktop and an agent's) share one server and one process; a missing server is `not_installed` with the command and the setting; topmost marker, per-project override and a project switched off; `max_servers` makes room or refuses; servers are `service` nodes in `resources.list` and die with the daemon; closing a connection closes its documents. `crates/cli/tests/it/lsp.rs`: `hutan diagnostics` with no desktop, the trust prompt and `hutan lsp trust|stop|restart`, a missing server's install command, an agent's CLI cannot trust, and the MCP `diagnostics` tool on a git checkout (changed file, direct importer, an untouched file left alone). Unit tests: the catalog, root detection, trust, position conversion, import patterns, `git status` parsing. Real servers never run in the gate; the manual check (rust-analyzer on this repository under supervision, its RSS in `hutan ps`) is recorded in the slice report.
