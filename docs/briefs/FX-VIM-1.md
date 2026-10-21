# FX-VIM-1 — Vim in file tabs: live status, search prompt, layouts, splits

Agent: implementer-opus (Claude Opus 5.5)   Depends on: E-8, E-10 merged   Branch: `slice/FX-VIM-1`

**Contract ownership:** none.

## Context
The end-to-end pass `docs/spikes/2026-10-23-e2e-usertest.md` (top issues 3–6, B2–B4) found, with Vim on in a file tab:
1. **Stale status bar:** the mode chip stays NORMAL in Insert (the caret does turn into a bar); `Ln/Col` updates only after a mouse click or `:` — `w`, `0`, `j` leave it frozen. A click past the end of a line in Normal mode puts the caret past the last character (`Col 16` on a 15-char line; Vim clamps to the last character).
2. **`/` and `?` show no prompt** while typing; `:` shows an ex line. Vim and Zed show the search line as you type (and highlight matches incrementally when `incsearch` — at least show the line).
3. **Non-Latin layouts:** Normal-mode commands match `key_char` (`code_view/modal.rs:118`), so with a Russian layout every command key is Cyrillic and does nothing. Map Normal/Visual/Operator-pending keys by the physical key (the US-QWERTY character of the key code) when the produced character is not ASCII, the way Zed binds by key; Insert mode keeps the produced text. Also add `editor.vim_langmap` only if trivial — otherwise out.
4. **`:sp`:** the new split starts at line 1 (Vim keeps the cursor line and scroll); in the lower pane the ex line is not visible and `:q<CR>` typed there did not close the pane, while the upper pane works.
5. **Settings sheet:** turning Vim on while the Settings sheet is open puts the *Settings search field* into NORMAL (first Escape) and after closing the sheet the editor has no focus caret until clicked. The row's provenance line shows only "You" (clipped).
Friction to decide inside the slice: `Vim uses the system clipboard` defaults **On** — Zed's default is `always`, Vim's is the unnamed register; keep On (Zed parity) but say so in the setting's description.

Read `AGENTS.md`, ADR 0012 amendment point 6, E-8's brief and code (`crates/code_view`, `crates/vim`, `crates/desktop/src/workbench/vim_ui.rs`, the editor status items), `docs/process.md` § UI checks, the IME/Escape rule from TI-1.

## Acceptance
GPUI tests (real key events, state-waiting): the mode chip shows INSERT/VISUAL/NORMAL after each switch; `Ln/Col` follows `w 0 j G` without a click; Normal-mode click past EOL clamps; `/foo` shows `/foo` while typing and Escape clears it; with a Russian layout simulated (key code for `d` producing `в`) `dd` deletes a line and Insert-mode typing still inserts Cyrillic; `:sp` keeps the cursor line in both panes, the lower pane shows its ex line and `:q` closes it; toggling Vim in Settings leaves the settings search field in plain mode and the editor gets focus back on close. Vim differential fixtures stay green. Cua screenshots: `/` prompt, Insert chip, `:sp` lower pane ex line. `just check` green; commit per step, messages end `(FX-VIM-1)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
None.

## Out of scope
Tree-sitter text objects (E-9); new Vim commands; `docs/status.md`, `docs/roadmap.md`.
