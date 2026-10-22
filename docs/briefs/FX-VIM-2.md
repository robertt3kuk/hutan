# FX-VIM-2 — Vim's own control keys (⌃[ = Esc and the rest) and user mappings

Agent: implementer-opus (Claude Opus 5.5)   Depends on: FX-VIM-1, E-12b merged (E-12b owns Insert-mode ⌃N/⌃P for completion)   Branch: `slice/FX-VIM-2`

**Contract ownership:** none.

## Context
The user (2026-10-23): "Vim mode must have its own key combinations — at minimum ⌃[ equals Esc." Today `crates/vim` understands only `⌃R ⌃V ⌃O ⌃I` in Normal mode and `⌃U ⌃C` on the `:` line (`command.rs:635-641`, `vim.rs:971-972`); every other `⌃` key is "Not supported" or falls through to the app. Behaviour is specified from Vim's documentation (`:help index`, `insert.txt`, `cmdline.txt`, `windows.txt`, `change.txt` §CTRL-A) and verified with differential fixtures recorded from real Vim (`xtask vim-record`, `crates/vim/tests/fixtures`), as for E-7b. Read ADR 0012 amendment point 6, `docs/editor.md` (Vim matrix), FX-VIM-1's key reading by US key name (`Vim::takes_text`), `crates/desktop/src/workbench/vim_ui.rs`, the keymap layer (`keymap*.rs`, keymap bases Zed/Hutan, contexts).

## Scope
1. **⌃[ is Escape everywhere Vim is on**: file tabs, chat composer, Start sheet and every text input that uses Vim; in Insert, Visual, Operator-pending, Replace, the `:` line and the `/` `?` prompt. Same for **⌃C** (Vim: leaves Insert, cancels pending/command line). Works on non-Latin layouts (physical `[` key; Russian layout types `х`).
2. **Insert mode**: `⌃W` delete word before the caret, `⌃U` delete to the start of the inserted text/line, `⌃H` backspace, `⌃J`/`⌃M` newline, `⌃T`/`⌃D` indent/dedent the line, `⌃O` run one Normal command and return, `⌃R {reg}` insert a register (`⌃R ⌃W`-style variants out), `⌃A` insert the last inserted text, `⌃E`/`⌃Y` copy the character below/above. `⌃N`/`⌃P` stay with completion (E-12b).
3. **Normal / Visual**: `⌃D`/`⌃U` half page, `⌃F`/`⌃B` page, `⌃E`/`⌃Y` scroll a line keeping the caret, `⌃A`/`⌃X` increment/decrement the number at or after the caret (decimal, hex `0x`, negative; Visual `g⌃A` sequences), `⌃^` alternate file, `⌃G` file info in the message line, `⌃]` go to definition (LSP, as `gd`) and `⌃T` back, `⌃L` no-op redraw, `⌃W` window commands mapped to Hutan panes: `s` `v` split, `h j k l` focus, `w` next, `q`/`c` close, `o` only, `=` equalize.
4. **Command line and search prompt**: `⌃W` delete word, `⌃U` clear, `⌃H` backspace, `⌃R {reg}` insert register, `⌃P`/`⌃N` and ↑↓ history.
5. **Precedence**: when Vim is on and the focus is a Vim text context, these keys go to Vim before app/keymap bindings (a Vim keymap context, like Zed's `vim_mode` contexts); with Vim off nothing changes. Document conflicts that were app bindings (e.g. a `⌃` shortcut in the Hutan base) and how to get them back.
6. **User mappings**: settings `editor.vim_mappings` (Insert, Normal, Visual) — key sequence → key sequence, non-recursive like `inoremap`/`nnoremap`, with `editor.vim_timeout_ms` (default 1000, Vim's `timeoutlen`) for multi-key sequences such as `jk → <Esc>`; validated with a clear Problems entry; shown on the Settings Keyboard page with the built-in Vim keys listed (searchable).
7. Docs: `docs/editor.md` Vim matrix (every key above with ✓), `docs/design.md` keyboard section, Settings labels.

## Acceptance
Differential fixtures recorded from real Vim for the Insert, Normal and Visual control keys that act on text (⌃W ⌃U ⌃H ⌃T ⌃D ⌃A ⌃E ⌃Y ⌃O ⌃R in Insert; ⌃A ⌃X incl. hex/negative/Visual sequences; ⌃D ⌃U ⌃E ⌃Y cursor positions where the harness can express them) replay green; GPUI tests with real key events: ⌃[ leaves Insert in a file tab, in the chat composer and on the `:` line, also with a Russian layout simulated; ⌃C likewise; ⌃W s/v/h/l/q drive panes; ⌃] / ⌃T jump with the fake LSP server; a `jk → <Esc>` mapping works and a lone `j` types after the timeout; with Vim off, ⌃[ and ⌃W keep their non-Vim meaning. Cua screenshot of the Settings Keyboard page with Vim keys. `just check` green; commit per item, messages end `(FX-VIM-2)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
None.

## Out of scope
Vimscript, `:map` commands at runtime (settings only), tree-sitter text objects (E-9); `docs/status.md`, `docs/roadmap.md`.
