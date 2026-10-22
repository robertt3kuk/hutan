# FX-E2E2-C — tabs, Files/Changes status, small editor and launcher fixes

Agent: implementer (Claude Sonnet 5.5)   Depends on: E-13a, CL-SETTINGS merged   Branch: `slice/FX-E2E2-C`

**Contract ownership:** none.

## Context
Second e2e pass `docs/spikes/2026-10-23-e2e-usertest-2.md` (findings 4–8, 10, B4, T9; shots in `~/hutan-scratch/e2e2/shots/`).

## Items
1. **Active tab into view** (shot 67): opening a file via ⌘P when the tab strip overflows shows no tab for it; scroll the strip so the active tab is visible on every activation; long chat titles are capped (ellipsis at a max tab width, full title in tooltip).
2. **Changes tab lists only the first of 4 files** (shot 85): find why only `app.py` renders ("1 of 4"); fix; GPUI test with 4 changed files. The Changes right-panel row shows the file count (`4 files · +4 −0`).
3. **Files panel git status** (shot 12): modified / added / untracked / deleted decorations (letter + tint, not colour only) from the existing worktree status already loaded for Changes (no new full diff per render; reuse the cached status, refresh on `fs.changed`/status refresh); folders show a dot when they contain changes. File icons by type if the icon theme has them; else unchanged.
4. **Vim toggle in Settings** (B4, shots 14–16): toggling Vim moves the editor caret one column left and leaves no visible caret until a click. Toggling must not change the editor's selection, and closing the sheet returns focus with the caret drawn.
5. **Ex line** (shot 33): shows `: q` with a space and overlaps the editor's bottom row; render `:q` and reserve its row.
6. **Start sheet default** (shot 44, T9): on a git repo where an agent harness is ready, the *What* default is the last used kind (persisted), falling back to *Agent chat* when a harness is ready, else *Plain terminal*. First-run *Open a folder* panel starts at `~`.
7. **Settings row help** (finding 9): truncated help text shows in full in a tooltip and when the row has keyboard focus (CL-SETTINGS intended this — verify it works; fix if not).

## Acceptance
GPUI tests for 1, 2, 3, 4, 6; screenshots before/after for each item (dark; light for 3) → `~/hutan-scratch/fxe2e2c/shots/`. `just check` green; commit per item, messages end `(FX-E2E2-C)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
Everything else; `docs/status.md`, `docs/roadmap.md`.
