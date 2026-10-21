# 0012. Embedded code editor (re-scopes the "no editor" non-goal)

- Status: accepted, option B (2026-10-17: the project is MIT OR Apache-2.0, so Zed's GPL-3.0 editor crates are out; Hutan builds its own editor on permissively licensed crates); **amended 2026-10-21** (below, accepted with the user)
- Date: 2026-10-14
- Milestone: M11 (see roadmap)

## Context
The user wants Hutan to work as an editor too: open files next to terminals and agent sessions, LSP on demand, Vim mode, tree-sitter highlighting and structure, one theme across chrome, terminal, and code — "a mini Zed inside the ADE". `vision.md` previously listed "Code editor / LSP host" as a non-goal; this ADR re-scopes it.

Constraints:
- Zed's `editor`, `language`, `lsp`, `vim`, `project`, `workspace` crates are **GPL-3.0** (only `gpui`/`gpui_platform` are Apache-2.0) and depend on each other and on Zed's settings/project model; linking them makes Hutan GPL-3.0 and couples us to Zed's internals at the pinned rev.
- Warp's equivalents are AGPL-3.0 (ideas only).
- Permissive building blocks exist: `tree-sitter` + grammars (MIT), `ropey` (MIT), `lsp-types` (MIT), `async-lsp`/own JSON-RPC client.

## Options
| Option | Pros | Cons |
|---|---|---|
| A. Reuse Zed crates (`editor`, `language`, `lsp`, `vim`) | Fastest path to a very capable editor | Hutan becomes GPL-3.0; heavy dependency graph; tight coupling to Zed internals and its bump cadence |
| **B. Own editor on GPUI with permissive crates** (rope + tree-sitter + LSP client + own Vim layer) | License freedom; scoped to what an ADE needs; shares our theme/settings/panel model natively | More work; Vim/LSP completeness grows incrementally |
| C. External editor only (open in Zed/VS Code) | Zero cost | Not what the user wants |

## Decision: B (the license is MIT OR Apache-2.0)
- New UI crate `code_view` (GPUI) + non-UI crates `buffer` (rope, edits, undo, selections), `syntax` (tree-sitter: highlighting, folding, structural selection, indentation), `lsp_client` (process supervision, JSON-RPC, capabilities, diagnostics, completion, hover, go-to-definition, references, formatting, rename).
- LSP servers run **in the daemon** (`runtime` supervision, like agents) so agents and every client share diagnostics; started on demand per workspace + language, stopped when idle. Server config in settings (`languages.<lang>.lsp`).
- Editor items are workbench items (`ItemRef::File { path }`) — split next to terminals, movable like everything else; one `Theme` drives chrome, terminal palette, and syntax scopes.
- Vim mode is a setting (`editor.vim_mode`), implemented as a modal layer over editor actions (normal/insert/visual, motions, operators, text objects incl. tree-sitter ones, registers, `.` repeat, `:w`/`:q`).
- Files are read/written by the desktop client; agent edits arrive through the daemon and reload open buffers (conflict prompt when dirty).

## Consequences
- `vision.md` drops "Code editor / LSP host" from non-goals; the editor is a first-class surface.
- The license is decided (MIT OR Apache-2.0, 2026-10-17): no Zed editor crates; the V-1 vim layer (`crates/desktop/src/workbench/chat/vim.rs`) is the starting point for the editor's modal layer.

## Amendment 2026-10-21 (accepted with the user)

Evidence: `docs/spikes/2026-10-21-editor.md`. The user chose option **B′** and every recommendation of its §4.11.

1. **Milestone** is M11 (the header said M12).
2. **Crates:** `buffer` (merged E-1: `crop` with the UTF-16 metric), `syntax` (merged E-2: tree-sitter, grammars behind cargo features), `vim` (merged E-7a: extracted from `chat/vim.rs`, generic over `VimHost`; the chat composer and every text input use it through a `TextInput` adapter), `lsp_client`, `code_view` (GPUI). "`runtime` supervision" means the `agents/src/jsonrpc.rs` child-process pattern; no `runtime` crate.
3. **File I/O goes through the daemon** (`fs.read`, chunked `fs.write` with an if-match precondition, `fs.stat`/`list`/`find`/`search`, `fs.watch` + `fs.changed` with agent-origin attribution; additive protocol minor, capabilities `fs`, `fs.watch`; contract sketch in the spike §4.4). This replaces "Files are read/written by the desktop client": permissions are enforced in the daemon, agents and the CLI need the same access, and remote clients (M9) cannot read local disks. The desktop holds the editable buffer; the daemon holds only bounded text mirrors of open documents for LSP (`doc.*`, capability `doc.sync`). Agent-originated `fs.write` goes through `[control]` approval.
4. **LSP configuration is split:** `daemon.toml [lsp]` / `[lsp.servers.<id>]` (command, args, env, root markers, languages, initialization options, idle stop, limits) versus desktop `editor.*` and `languages.<lang>.*`. The single `languages.<lang>.lsp` key is dropped.
5. **LSP servers are found, never fetched:** from the login environment (B-1) and `daemon.toml`; the first start in a folder requires a trust decision (`[lsp] trust = "ask"` by default); a missing server shows an install card, never a download.
6. **Vim** is first-class before the M11 exit demo (block visual, macros, registers, marks, ex commands, tree-sitter text objects: E-7b, E-9 on the critical path). `editor.vim_mode` stays default-off with a first-run question. Behaviour is specified from Vim/Neovim documentation and verified by differential fixtures recorded from a real Vim; Zed's GPL `vim` crate is not read or ported; MIT/Apache sources may be borrowed with attribution.
7. **GPUI Kit** (Apache-2.0) is not adopted (no Vim, private 12k-line edit state, own theme system, weekly breaking releases). Revisit trigger: the end of E-5 (spike §4.1).
8. **⌘P** becomes one palette with *Files* and *Sessions* sections (Zed base); ⌘J keeps the jump palette in the Hutan base.
9. **Non-goals** per spike §3.2; budgets B11–B16 per spike §4.8, measured in `docs/perf.md`.
