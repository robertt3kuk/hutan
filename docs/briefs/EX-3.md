# EX-3 — filter as you type in the explorer

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-3`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-27: a real file explorer ("that's the point of bringing an editor inside"). Spike `docs/spikes/2026-10-27-file-explorer.md` §3 and §4 define this slice; EX-1 (tree polish), EX-2 (peek), EX-6 (daemon fs ops) and EX-7 (explorer file operations: name field, Trash, copy/paste, drag, undo; `panels/files/{edit,ops,drag}.rs`) are merged. Decisions: Trash only; file ops are not MCP tools; Open Editors auto (≥ 2 file tabs). FX-WORK-1 already made `fs.find` AND-match whitespace-separated words.

Implement §4 EX-3 items (2)–(5) (item 1 is done): filter field in the Files header (⌘F with the tree focused, or just start typing), local instant matching over loaded rows, `fs.find` scoped to the root/folder with debounce and stale-drop (≤ 200 results), tree projection with synthesised ancestors and highlight, ↓ into results, Esc restores the tree and selection; wire EX-1's "Showing the first 5 000 — filter in this folder" row and *Find in Folder* (Search panel include glob).

## Acceptance
Projection unit tests; GPUI typing test with a slow fake daemon (no frame waits on the daemon); real-daemon test at 100k files; Cua screenshots; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-3)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex3/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
