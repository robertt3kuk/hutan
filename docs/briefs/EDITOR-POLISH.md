# EDITOR-POLISH — git gutter, blame width, LSP trust bar, scroll clamp

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EDITOR-POLISH`

**Contract ownership:** none expected (reuse `git.file`/`worktree.diff`/`worktree.stage`); if wire is needed, stop and report.

## Context
Workability evaluation `docs/spikes/2026-10-27-workability.md` (user 2026-10-27; screenshots `~/hutan-scratch/e2e6/shots/`): editor findings that block daily use after soft wrap (EDITOR-WRAP, merged).

## Items (in commit order; each with a regression test)
1. **F-11 git change marks in the gutter**: added / modified / removed bars beside line numbers against the index (or HEAD when nothing staged — say which, VS Code uses the index), updated on edit (diff of the buffer text against the base text fetched once via `git.file`/the existing diff path, recomputed off the frame with a debounce), click a mark → inline hunk peek with *Revert hunk* (through the buffer, undoable) and *Stage hunk* (STAGING's `worktree.stage`), ]c / [c (Vim) and ⌥F5/⇧⌥F5 to step; works with soft wrap (mark on every row of the line). Setting `editor.git_gutter` (default on).
2. **F-09 blame width**: the blame column sizes to content with a cap of ~32 cells (author initials or short name · relative date · as much subject as fits), the full text on hover; never more than 30 % of the editor width; wraps nothing.
3. **F-10 LSP trust bar**: one bar per window per folder (not per pane/split); not shown for languages whose configured server is a plain formatter/linter that runs no project code — or rather: wording says exactly what will run (server name and command) and appears only when a server for that file type is configured and found; *Not now* is remembered for the session and reachable again from the status bar (`untrusted` item click → the same choice; closes the known "no in-window way back to Trust" gap).
4. **F-19 scroll past end**: scrolling stops with the last line at the bottom plus `editor.scroll_beyond_last_line` (default half a screen, VS Code-like), never a blank view; returning to a tab restores a valid offset.
5. Docs: `docs/editor.md`, `docs/lsp.md` (trust bar), `docs/design.md`.

## Acceptance
Unit tests for the gutter diff (insert/modify/delete at edges, CRLF, wrap); GPUI tests: marks appear after typing and clear after undo, peek revert/stage, step keys; blame width cap; one trust bar in a split, status-bar way back; scroll clamp; Cua screenshots on `~/hutan-scratch/e2e6/repos/ripgrep` (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EDITOR-POLISH)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/editorpolish/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
