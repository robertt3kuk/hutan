# TERM-FIND — Find in terminal output and scrollback

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/TERM-FIND`

**Contract ownership:** **delegated**: one additive protocol minor if the search must run in the daemon (it owns the scrollback): `terminal.search {terminal_id, query, case?, regex?, limit≤1000, from?}` → matches (row, cols, block id), bounded; capability `terminal.search`; golden fixtures appended.

## Context
The most-engaged terminal request across Ghostty, Warp and cmux (Ghostty #189, 1221 reactions; GhD #3708; `docs/spikes/2026-10-25-user-needs/terminals.md`). Hutan has no find in a terminal (checked in `crates/terminal_view`). Scrollback lives in the daemon (bounded, ADR 0004); the view holds what is loaded; `terminal.block_text` already reaches beyond the loaded scrollback.

## Items (in commit order)
1. **Search where the text is**: the daemon searches the session's scrollback + screen (plain text per logical line, wrapped rows joined), case-insensitive by default, optional case-sensitive and regex (`regex` crate, size-limited), bounded results and time (cancel on a new query); results are positions stable against new output (absolute row index).
2. **Find bar** in a terminal (⌘F when focus is in a terminal; ⌘G / ⌘⇧G or Return / ⇧Return next/previous; Esc closes and returns focus to the terminal): the same look as the editor's find bar (`Aa`, `.*`, `n of m`), matches highlighted in the visible rows, the current one stronger, scrolling to a match loads that part of the scrollback; new output keeps the matches and count current (debounced). Block-aware option: *Only in this block's output* when a block is selected.
3. **Keyboard parity**: in Vim-enabled setups the terminal is unaffected (Vim keys apply to text inputs only); ⌘F in a terminal never reaches the shell.
4. CLI: `hutan terminal search SESSION QUERY [--regex] [--case]` prints `row: line` matches. MCP not needed.
5. Docs: `docs/design.md` (terminal find), `docs/protocol.md` if the wire changed.

## Acceptance
Daemon tests: matches across wrapped rows, beyond the loaded scrollback, regex, case, bounds and cancel; GPUI tests: ⌘F opens, typing highlights, next/previous wrap, scroll to an old match, new output updates the count, Esc returns focus; B4 keystroke budget unchanged when the bar is closed. CLI test.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(TERM-FIND)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/termfind/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
