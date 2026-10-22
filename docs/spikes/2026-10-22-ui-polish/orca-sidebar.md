# Orca sidebar structure and Hutan gap (2026-10-22)

Scope: how Orca (github.com/stablyai/orca, MIT) structures its left and right sidebars, what Hutan does today, and what would attach the sidebar to the active workspace or folder and move Mawas Memory into it. Read-only; nothing in Hutan was edited.

## Method and limits

- Orca source: partial blobless clone at `~/proj/research/stablyai_orca`, HEAD `de2700d` (2026-10-22). Full shallow clones timed out twice. Files were read with `git cat-file -p HEAD:<path>`. Paths are relative to `src/renderer/src/` unless noted.
- Right-sidebar and card line numbers come from the 2026-10-18 spike (`docs/spikes/2026-10-18-orca-layout.md`) and were not re-read. They are marked "prior spike".
- Issue searches (read-only `gh`): `gh search issues --repo stablyai/orca "sidebar"` returned `[]`. `gh search issues --repo stablyai/orca "sidebar worktree"` returned 4 issues. Reaction counts came from `gh api repos/stablyai/orca/issues/<n> -q .reactions.total_count`.
- README hero image: `docs/assets/readme-hero.jpg`, alt text "Orca desktop app running agents in parallel worktrees". I did not view the image.

## Part 1: Orca's sidebars

**Left sidebar: a worktree list with a body switch.**
- `components/sidebar/index.tsx` sets `MIN_WIDTH = 220`, `MAX_WIDTH = 500` (L37–38). The body comes from `sidebarBody ?? 'workspaces'` (L61).
- Stack (L171–221): `SidebarNav`, `SidebarHeader`, daemon banners, one body, `SetupScriptPromptCard`, `SidebarToolbar`. `sidebarBody === 'agents'` renders `SidebarAgentsList` (L181–195). Otherwise it renders `WorktreeList` (L196–208).
- `SidebarNav.tsx` (L51–93) holds page openers: `openArtifactsPage`, `openSkillsPage`, `openAutomationsPage`, `openMobilePage`, and the worktree palette. These set `activeView`, a center-page switch.
- `SidebarHeader.tsx` (L23–42) titles the section "Projects" when `groupBy === 'repo'`, else "Workspaces". Its button (L83) toggles `sidebarBody` between `'workspaces'` and `'agents'`.
- `WorktreeList.tsx` reads `activeWorkspaceKey`, `activeWorktreeId`, `groupBy`, and `projectGroups` (L72–98). The active row comes from `getActiveSidebarWorkspaceId` (L80).
- Preferences (`store/slices/ui/ui-slice-contract-preferences.ts`): `sidebarBody: 'workspaces' | 'agents'` (L28), `groupBy: 'none' | 'workspace-status' | 'repo' | 'pr-status'` (L30).

**Attachment: the scope key.**
- The active container is a `WorkspaceKey`, `worktree:<id>` or `folder:<id>` (`src/shared/folder-workspace-types.ts:13–17`).
- `getActiveSidebarWorkspaceId` (`src/shared/workspace-scope.ts:43–55`) prefers the folder key, then the worktree key, then the bare `activeWorktreeId`. Its comment: "folder workspaces are tracked by the scoped active key, while older worktree-only paths still read activeWorktreeId."
- `FolderWorkspace` (`folder-workspace-types.ts:19–23`) has `projectGroupId` and `folderPath`. A folder is a child of a project group, as a worktree is a child of a repo.

**Right sidebar: tabs scoped to the active container.**
- `components/right-sidebar/index.tsx` (prior spike L31–235) takes `rightSidebarOpen`, width, and `activityBarPosition` (L39–44). Tabs come from `useRightSidebarActivityItems` (L47–53).
- That hook sets `activeFolderWorkspaceKey`, non-null only for folder workspaces (`use-right-sidebar-activity-items.ts:50`, L149). The "Attached worktrees" tab is `folderOnly: true` (L91–102).
- Comment at `index.tsx:83–93`: panels "react to activeWorktreeId changes via store subscriptions… keeping the component instance alive across switches", and "The right sidebar is back to tab-only content."

**Issues (demand signal, read-only):**

| # | Title | State / date | Comments | Reactions |
|---|---|---|---|---|
| 11783 | Collections: cross-repo workstream sections in the sidebar | open, 2026-07-31 | 2 | 2 (both +1) |
| 21825 | Sidebar drag/kanban pin path | open, 2026-09-20 | 1 | 0 |
| 30788 | Workspace board status badge on sidebar worktree cards | open, 2026-10-19 | 0 | 0 |
| 31487 | Hide project name on sidebar worktree cards | open, 2026-10-21 | 0 | 0 |

Links: `https://github.com/stablyai/orca/issues/<n>`. #11783 says "Project groups operate at the repo level" and proposes a visual-only section: "Purely visual and additive — no folders created on disk, no workspace state switching." Demand is thin. [INFERENCE] No issue asks for an assistant or memory surface in the sidebar.

## Part 2: Hutan today

Sources: `docs/design.md`, `docs/assistant.md`, `docs/orchestration.md`, `crates/desktop/src/workbench/`.

- **Left nav.** `design.md:84–88`: rows are Agents and Mawas, plus Tasks once a task exists. Default page is Projects. `sidebars.rs:129–196` (`nav_entries`) builds this: `agents` always (L134–143), `assistant` when enabled (L144–155), `tasks` (L158–167), then left-dock panels (L171–194). `NavTarget` is at L39–46.
- **Right rail.** `design.md:100`: Changes, Agents, History. ⋯ holds Requests and Resources.
- **Agents.** Center tab `agents-dashboard` (`design.md:90–98`; `model/mod.rs:37–38`).
- **Memory.** `assistant.md:103`: page `mawas-memory`, one center tab, opened from ⌘K or the *Memory ›* link in Today. Scopes are Global or Workspace (`assistant.md:80–107`).
- **Registration vs routing.** `panels/mod.rs:246–252` registers `mawas-memory` with `default_dock: Left`. `view.rs:1750–1767` (`open_panel`) sends Mawas pages and `AGENTS_PAGE` to the center via `open_panel_in_center`. `actions.rs:859–862` (`is_mawas_page`) lists `"assistant" | "mawas-memory"`. `assistant.rs:82–108` (`open_memory`, `memory_panel`) calls `open_panel("mawas-memory")`. The Left registration is overridden on every open path.
- **Per-container layouts.** `model/workspace.rs:73–100` (`switch_workspace`) parks the outgoing layout in `parked` and restores the target's, or `fresh_layout` (L104–131). [INFERENCE] Docks are part of `WorkspaceLayout`, so visible left panels already change per container. Not confirmed by a full read.
- **Scoped panels.** `panels/changes.rs` (L1–4) lists checkouts of the current workspace. `panels/files.rs` (L1–3) shows the active project. `panels/search.rs` (L1–2) searches it. `panels/memory.rs` (L1–12) says "as a tab of the center (one instance)".
- **Switch path.** `workspaces.rs:252` (`switch_workspace`), L262–302 (`show_workspace`).

## Part 3: The gap and where Memory fits

1. **Attachment exists in both at different levels.** Orca's sidebar lists the active project group's workspaces, and its right tabs depend on the container type (`activeFolderWorkspaceKey`). Hutan's layouts are already per container. The gap is which surfaces are sidebar surfaces. In Hutan, Mawas and Memory are center tabs. In Orca the center holds pages and the sidebar holds the list plus an Agents body.
2. **Pattern to copy: a body switch.** Orca's `sidebarBody` swaps the left body in place (`ui-slice-contract-preferences.ts:28`, `SidebarHeader.tsx:83`). Hutan can mirror it with a left body or a Memory row using the existing `NavTarget::Page(PanelId)` (`sidebars.rs:39–46`).
3. **Changes for Memory in the sidebar.**
   - `view.rs:1757–1759` (`open_panel`): stop routing `mawas-memory` to the center. Keep the center route only for an explicit "open as tab".
   - `actions.rs:859–862`: decide whether Memory stays a Mawas page for ⌘K grouping.
   - `assistant.rs:82–108`: point `open_memory` and `memory_panel` at the sidebar dock.
   - `panels/memory.rs:1–12`: revise the one-center-instance rule.
   - `design.md:84–88`, `:100`, and `assistant.md:103`: update the docs after the decision.
   - `nav_entries` (`sidebars.rs:171–194`) already lists left-dock panels, so Memory would appear once its dock is Left and routing changes. [INFERENCE]
4. **Scope.** Orca keys containers by `folder:`/`worktree:`. A sidebar Memory view would follow the active `WorkspaceKey` the way `getActiveSidebarWorkspaceId` does. [INFERENCE]
5. **Thin evidence.** Orca side is strong on file:line. Demand evidence is weak: no matching issue asks for an assistant or memory surface.

## Open items

- Confirm that `WorkspaceLayout` stores docks by reading `model/workspace.rs` and `model/types.rs:161` in full.
- Choose between a Left-dock sidebar body for Memory and the status quo (nav row opens a center tab). This is a design decision; the report does not make it.
- Re-read the right-sidebar line numbers from the prior spike if the right sidebar is redesigned.
