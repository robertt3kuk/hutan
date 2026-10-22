# SB-ATTACH — the sidebar is attached to the workspace or folder you are working in (Orca's model)

Agent: implementer (Claude Sonnet 5.5)   Depends on: FX-STATUSBAR, CL-POLISH-3 merged   Branch: `slice/SB-ATTACH`

**Contract ownership:** none expected (desktop only; per-container layouts already exist in `model/workspace.rs`). Stop and report if a wire change seems needed.

## Context
The user (2026-10-22): "the sidebar should be attached to the workspace or folder we are working on, as in Orca". Evidence: `docs/spikes/2026-10-22-ui-polish/orca-sidebar.md` (Orca: the left sidebar is a list of containers keyed `worktree:<id>` / `folder:<id>`; one is active; the center and the right sidebar follow the active container; a body switch swaps the left body in place) and `docs/spikes/2026-10-18-orca-layout.md`. Hutan today: a Projects tree listing every project and its sessions at once; the center layout is already per container (`switch_workspace` parks/restores `WorkspaceLayout`), Files/Changes/Search already use the active project — but nothing in the sidebar says which container is active, and the tree reads as global.

## Rule
One container is **active** at a time: a workspace, a project (folder or repo), or one of its worktrees. The sidebar shows it, the center shows its tabs, and every scoped panel (Files, Changes, Search, Problems, Agents rail, History filter, status bar `workspace` item) follows it and names it.

## Items (in commit order)
1. **Container list on top of the left sidebar**: rows (or Orca-like compact cards) for workspaces → projects → worktrees, each with name, branch (`⎇ main` / `wt feature/x`), and its attention glyph (Requests / working counts); the **active** container has the selected fill and is the only expanded one by default. Clicking another container switches to it (the existing `switch_workspace` / project activation path), the center swaps to that container's tabs, and the scroll position of the list stays.
2. **The active container's sessions** directly under it (terminals, agent chats, tasks), with the existing one-line rows and state lane; other containers stay collapsed with their counts. A setting `sidebar.show_all_sessions` (default off) restores the old everything-expanded tree for those who want it.
3. **Scoped panels say their scope**: Files, Changes, Search, Problems and the Agents rail show the active container's name in their header line (no duplicate headings — CL-POLISH-4 removes them; a small scope chip is enough) and refresh on switch without a flash (keep the old content until the new arrives).
4. **Keyboard**: ⌃Tab / ⌃⇧Tab or ⌘⌥↑/↓ (pick per keymap base; document) cycles containers; the jump palette (⌘J / ⌘P Sessions) switches containers too; switching never closes a session.
5. **Folder vs worktree** look distinct (folder glyph vs branch glyph), as Orca's `folder:` / `worktree:` keys; a non-git folder never shows an empty branch.
6. **Files on the right** (user, 2026-10-22; Orca puts Explorer on the right): the built-in presets place `files` in the right dock (first rail icon, before Changes), not the left; ⌘⇧E opens it there; layouts saved with the built-in default move it once (logged), a user-placed Files panel stays where the user put it.
7. Docs: `docs/design.md` (left sidebar, scoped panels, the rule, Files on the right), `docs/orchestration.md` (active container).

## Acceptance
GPUI tests: switching container swaps the center tabs and every scoped panel's content and scope label; only the active container is expanded; attention counts on collapsed containers; keyboard cycling; `show_all_sessions` restores the full tree; switching keeps sessions running. Cua screenshots dark + light: two projects with worktrees, before/after a switch → `~/hutan-scratch/sbattach/shots/`. `just check` green; commit per item, messages end `(SB-ATTACH)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
