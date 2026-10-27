# EX-8 — explorer roots and worktrees, Finder import

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-8`

**Contract ownership:** none (desktop; uses `fs.copy` with `picked_from` from EX-6).

## Context
User 2026-10-27: a real file explorer. Spike `docs/spikes/2026-10-27-file-explorer.md` §3.2, §4 EX-8 and EX-9; EX-1…EX-7 merged (tree, peek, filter, menu + multi-select, sections, file operations).

## Items (in commit order)
1. **EX-8 (1)** the scope chip's root switcher: checkouts (worktrees) of the project, other projects of the workspace, *Follow active container* (default); per-root caches from EX-1 reused.
2. **EX-8 (2)** `panels.files.roots = active | workspace`: with `workspace`, every project of the workspace is a stacked root row (collapsible) in one tree; filter (EX-3) and sections (EX-5) work across roots.
3. **EX-9** drop files/folders from Finder into a tree folder and ⌘V of Finder-copied files copy them in through `fs.copy` with `picked_from` (EX-7 already pastes; add the drop target, spring-loaded folders apply); a remote daemon refuses with the reason (`docs/remote.md`).
4. Docs: `docs/editor.md`, `docs/design.md`.

## Acceptance
GPUI tests over a real daemon: switch root to a worktree and back (caches kept), workspace roots stacked with filter and sections, a Finder drop (simulated external paths drop) copies into the folder with one undo step; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-8)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex8/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
