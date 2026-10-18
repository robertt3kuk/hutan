# Orca layout study (read-only scout of stablyai/orca 1.4.214, MIT)

Source for the OL-* slices (2026-10-18). Paths in the Orca sections are relative to a checkout of github.com/stablyai/orca.

## Summary

Orca is a **three-column workbench**, not a permanently docked assistant/chat application. Its left column navigates global pages and projects/workspaces; the center contains tabbed, splittable terminal/chat/editor/browser surfaces; the right column contains tool panels, including **Agent Session History**. **Agent Dashboard is a separate optional kanban drawer/pop-out, not a right-sidebar tab.**

References below are relative to `/Users/workie/proj/orca-src-git/`. `R` abbreviates `src/renderer/src/`.

```text
┌─────────────────────────┬─────────────────────────────────┬──────────────────────┐
│ traffic lights / Orca   │ tabs / tab-group headers        │ tool icons + collapse│ 36px
│ sidebar toggle, ← →    │                                 │                      │
├─────────────────────────┼─────────────────────────────────┼──────────────────────┤
│ Search                 │                                 │ Explorer             │
│ Tasks                  │ terminal / native agent chat    │ OR Source Control    │
│ [Skills]               │ editor / browser                │ OR Session History   │
│ Automations            │                                 │ OR Checks / Ports    │
│ [Agent Dashboard]      │ ── resizable split groups ──    │                      │
│ Orca Mobile            │                                 │ History:             │
│                        │                                 │ scope / search       │
│ Projects  [activity][+] │                                 │ scan / count / sort  │
│  project               │                                 │ project groups       │
│   workspace + agents   │                                 │ session rows         │
│  project …             │                                 │                      │
│ bottom toolbar         │                                 │                      │
├─────────────────────────┴─────────────────────────────────┴──────────────────────┤
│ usage/provider/account controls · updates · resources/ports · workspace controls│ 24px
└───────────────────────────────────────────────────────────────────────────────┘
       default 280px                 flexible                     default 350px
```

Square brackets indicate opt-in navigation entries. The **default right panel is Explorer**, not History. On landing/settings/tasks pages, a conventional titlebar spans the left+center area; in an active workspace, center tab groups extend to the top.

## 1. Regions, components, sizing, and interaction

| Region | Implementation and behavior |
|---|---|
| Overall shell | `R/app-shell/AppWorkspaceShell.tsx:90-270`. Nested left+center wrapper plus independent right-sidebar column. `ActivePage` selects settings, skills, artifacts, tasks, automations, activity, space, mobile, creation, or landing (`:67-87`). The terminal workbench remains mounted independently of page navigation. |
| Titlebar | `R/app-shell/TitlebarLeftControls.tsx:22-150`: macOS traffic-light reservation, optional Orca name, sidebar button, workspace/page back/forward. Windows/Linux use app logo and application-menu ellipsis. `R/assets/main.css:960-1010`: **36px** height, drag region, 1px divider; macOS traffic-light padding **80px / UI zoom factor**. Collapsed sidebar controls float over the center instead of consuming a column. |
| Left sidebar | `R/components/sidebar/index.tsx:36-41,151-257`: `SidebarNav`, `SidebarHeader`, scrollable `WorktreeList` or `SidebarAgentsList`, then setup prompt and fixed `SidebarToolbar`. Default **280px**, resize **220–500px**. Resize target is **12px** wide, centered over the seam; 1px highlight line. Closed width is **0**, not an icon-only rail. |
| Center | `R/components/TerminalSurface.tsx:1-43`: autosave controller → titlebar tabs → split workspace surfaces → legacy workspace surface → dialogs. `TerminalTitlebarTabs.tsx:18-124` passes terminal, editor, browser and simulator tabs to `components/tab-bar/TabBar.tsx`; supports close/close others/left/right, rename, colors, pinning, preview files, new surfaces, and pane expansion. |
| Split center | `R/components/TerminalSplitWorkspaceSurfaces.tsx:8-96`: one retained `WorktreeSplitSurface` per mounted workspace; layout and focused group determine visible tab groups. Inactive workspaces can retain/park their surfaces. Workspace group splits are distinct from terminal-pane splits. |
| Right sidebar | `R/components/right-sidebar/index.tsx:31-235`: default top activity strip; optional **40px vertical strip on the far right**, selected through context menu. Default durable width **350px**. Minimum **220px**; maximum is viewport width minus **320px** and any 40px side strip. `right-sidebar-width.ts:1-32`. Left-edge resize handle **4px**. Closed width **0**, shell retained but heavy panel content disconnected. |
| Bottom status | `R/app-shell/AppRootSurfaces.tsx:177-195`: conditionally mounted, reserves **24px**. `R/components/status-bar/StatusBar.tsx` delegates to `StatusBarSurface.tsx`; provider usage/account controls, usage roster popover, updates, skill updates, native-chat restart/resume, and optional resources/ports controls. Usage yields space first and collapses into overflow (`StatusBarSurface.tsx:136-223`). Status visibility defaults true. |

**Resize behavior:** `R/hooks/useSidebarResize.ts:20-30,82-165` clamps continuously, applies live DOM width through `requestAnimationFrame`, and commits the store width on mouseup/window blur. It does **not** implement drag-past-threshold collapse.

### Right tool icons: exact meaning

`R/components/right-sidebar/use-right-sidebar-activity-items.ts:76-148` defines the ordered strip:

1. **Explorer** — Lucide `Files`; file tree plus search subview.
2. **Agents** — custom `AgentSessionHistoryIcon`; routes to `vault`, whose panel title is **Agent Session History**. This is **history, not the live dashboard**.
3. **Attached worktrees** — Lucide `Workflow`; folder-workspace only.
4. **PR Checks** — `ListChecks`; folder-workspace only.
5. **Source Control** — `GitBranch`; Git workspaces only.
6. **Checks** — `ListChecks`; Git workspaces only.
7. **Ports** — `Plug`; conditional on reachable SSH-host context.
8. Installed plugin panels, when enabled.

`right-sidebar-top-activity-bar.tsx:38-156`: **36px** top strip; insufficient width moves icons into **More**, rather than horizontally scrolling. With custom Windows/Linux window chrome, navigation moves below the native-style window-control band.

### Keyboard defaults

`src/shared/keybindings/definitions-core-1.ts:124-141,181-250`:

| Action | Default |
|---|---|
| Left sidebar | `Mod+B` |
| Right sidebar | `Mod+L` |
| Explorer | `Mod+Shift+E` |
| File search | `Mod+Shift+F` |
| Source Control | `Mod+Shift+G` |
| Checks | Unassigned |
| Ports | macOS `Mod+Shift+I`; unassigned elsewhere |
| Agent Dashboard | Unassigned, remappable |
| Tasks | Unassigned |

`Mod` is the platform command/control modifier. History has no shortcut advertised in its activity-item definition. Dashboard handles `Escape` to dismiss, while respecting nested overlays; dashboard-local `Mod+K` focuses search (`AgentKanbanBoard.tsx:191-215`).

## 2. Left navigation and workspace rows

### Navigation

`R/components/sidebar/SidebarNav.tsx:19-47,86-296`:

- **Search** opens the **worktree/browser-tab jump palette**; it is not the right-sidebar file-content search.
- Setup Guide entry and Tasks.
- Artifacts: only when `showArtifactsButton === true`.
- Skills: only when `showSkillsButton === true`.
- Automations: shown unless explicitly disabled.
- Agent Dashboard: only with `experimentalAgentDashboardPopout === true`.
- Orca Mobile: shown unless explicitly disabled; can show **New** badge.
- Several entries support context-menu **Hide from sidebar**.

`SidebarHeader.tsx:23-90`: section title **Projects** when grouped by repo, otherwise **Workspaces**; activity/bell button switches the sidebar body between `workspaces` and `agents`. It does not open a full conversation.

### Rows and badges

The rendering chain is:

`WorktreeList` → `WorktreeCard` → `worktree-card-surface.tsx` → parent content/header/meta/secondary rows.

- `worktree-card-header.tsx:94-180`: project identity chip/icon, SSH/runtime host indicator, editable workspace title; title **13px / 20px line height**.
- `worktree-card-surface.tsx:49-88`: vertically stacked rounded cards, not a fixed-height flat list. Title-only vertical padding **8px**; richer cards **5px top / 6px bottom**; left inset **4px**, right padding **6px**, rounded-lg.
- `WorktreeCardStatusSlot.tsx:44-74,121-157`: **20px status lane**, **13px** branch/review/sleep glyphs; unread is an overlaid **6px amber dot** with 2px sidebar-colored ring. Working/permission states take precedence over the unread glyph.
- Title weight/contrast also distinguishes unread from read (`worktree-card-header.tsx:167-180`).
- Branch/PR/sleep identity and status can share the leading lane; richer branch/issue/review/host metadata is presentation-dependent, not mandatory in every row.
- `worktree-card-secondary-rows.tsx:87-98`: live agent rows appear inline beneath the workspace title; expandable lineage children are supported.
- `WorktreeCardMeta.tsx:214-400`: hover/detail content includes branch/workspace identity, linked Linear/Jira issue details, labels, notes and other metadata; issue titles **13px**, small label badges **9px**.

## 3. State and persistence

**Store:** React components use `useAppStore`, a Zustand-style central application store.

| State | Source |
|---|---|
| `sidebarOpen`, `sidebarWidth` | `R/store/slices/ui/ui-slice-agent-actions.ts:46-50`; initial renderer defaults true / 280. |
| `rightSidebarOpen`, `rightSidebarWidth`, `rightSidebarTab`, `rightSidebarExplorerView`, route request ID, effective tab, `activityBarPosition` | `R/store/slices/editor/actions/right-sidebar-state.ts:10-55,146-147`. Activity position defaults `top`. |
| Durable defaults | `src/shared/constants.ts:190-201`: terminal page, left 280, right open, Explorer/files, right 350, group by repo. Renderer slice’s pre-hydration right defaults are false/280; **do not mistake those for durable launch defaults**. |
| Hydration | `R/store/slices/ui/ui-slice-hydration-actions.ts:92-116`: sanitize widths; absent open flags default true; normalize right route. |
| UI persistence | `R/app-shell/use-persisted-ui-writer.ts:142-215`: persists sidebar widths/open state, selected right tab/subview, grouping/sorting, filters, explorer state and agent read/unread acknowledgements. **150ms debounce**, diffed against the last persisted baseline to avoid sync echoes. Active page is a separate small profile preference. |
| Durable schema | `src/shared/persisted-ui-state-types.ts:36-46,136-140`. |
| Center selection | `TerminalTitlebarTabs.tsx:24-59,92-109`: active tab ID/type, editor file ID, browser ID, tab order and expanded pane state are separate fields. `TerminalSplitWorkspaceSurfaces.tsx:13-27,57-80`: layout per workspace and focused group per workspace. |
| Sidebar activity | `SidebarHeader.tsx:23-27`: `sidebarBody` selects workspaces/agents. Query, scroll position, and collapsed activity groups also have component-local state (`sidebar/index.tsx:63-90`). |

`activityBarPosition` is store state; it is **not part of the cited persisted-UI write baseline**, so persistence should not be assumed merely from its presence in the store.

## 4. Visual language

Primary theme source: **`R/assets/main.css:172-265,313-375`**.

| Token | Light | Dark |
|---|---|---|
| `--background` | `#fff` | `#0a0a0a` |
| `--foreground` | `#0a0a0a` | `#fafafa` |
| `--editor-surface` | `#ffffff` | `#1e1e1e` |
| `--worktree-sidebar` | `#f5f5f5` | `#2a2a2a` |
| `--worktree-sidebar-accent` | `#eaeaea` | `#353535` |
| `--sidebar` (right) | `#fafafa` | `#171717` |
| `--sidebar-accent` | `#f5f5f5` | `#262626` |
| `--muted-foreground` | `#737373` | `#a1a1a1` |
| `--border` | `#e5e5e5` | white at **7%** |
| `--ring` | `#a1a1a1` | `#737373` |

- **Geist** UI font with system fallbacks; monospace starts with **SF Mono**. Base radius token **0.625rem**.
- Mostly **Lucide React** icons; custom agent/provider/project glyphs and custom history icon.
- Navigation: outer padding **8px**, row gap **2px**, row padding **8px horizontal / 6px vertical**, **13px medium** labels, **16px** icons. Inactive icon/label foreground opacity **30% / 60%**; hover fill foreground **8%**. Selected rows use worktree-sidebar accent; stroke weight rises **1.75 → 2.25** (`SidebarNav.tsx:86-179`).
- Section header: **32px** high, **12px semibold**, muted foreground at 80% (`SidebarHeader.tsx:49-57`).
- Workspace hover: light foreground mix **4%**, dark sidebar-accent mix **40%**. Active primary card: light foreground mix **8%**, border worktree-border **40%**, subtle shadow (`main.css:1413-1444`). Multi-selection gets a separate border/ring treatment.
- Empty states are compact muted copy rather than oversized illustrations in history/dashboard: dashboard column **“None”**, **11px**, with padding (`AgentKanbanBoard.tsx:104-109`); missing history preview **“No conversation preview available”** (`AiVaultSessionRow.tsx:216-233`).

## 5. Agent chat, History, and Dashboard

### Center conversation

A terminal leaf can become a **native conversation UI in the same center pane**:

- `R/components/terminal-pane/TerminalPaneNativeChatPortal.tsx:72-90`: portals `NativeChatView` into the selected terminal leaf.
- `NativeChatPaneCover.tsx:8-23`: absolute full-pane chat cover, chat-canvas background, owns focus/paste.
- `R/components/native-chat/NativeChatView.tsx:12-31,34-89`: structured session or terminal-bridge resolution; renders conversation/composer UI for the selected agent/session, with a switch-to-terminal path.
- `TerminalPaneHeaderOverlay.tsx:258-274`: **Show terminal** control and resume-in-new-native-chat affordance.
- Chat styling is intentionally calmer than chrome: maximum content width **46rem**; theme-derived canvas, user-message, code and composer mixes (`main.css:199-213,316-330`).

Thus native chat is **workspace/tab/pane content**, not a global right-docked assistant.

### Right: Agent Session History

Main implementation: `R/components/right-sidebar/AiVaultPanel.tsx`; its constituent components supply the exact requested visual structure:

- `AiVaultPanelHeader.tsx:87-165`: **Agent Session History**, subtitle **Indexed history** / **Resume past sessions**, host/view/refresh controls, segmented scope control and **32px** search field. At panel widths below **300px**, title shortens to **Agents**.
- `AiVaultScanProgressStrip.tsx`: scan-progress/freshness strip.
- `AiVaultSessionListBar.tsx:21-31,50-66`: **“shown of loaded sessions”** and sort dropdown in **32px** bar.
- `AiVaultSessionVirtualList.tsx`, `AiVaultVirtualRow.tsx`: virtualized grouped list.
- `AiVaultSessionRow.tsx:145-170,216-252`: **98px minimum collapsed row**, **12px horizontal / 8px vertical padding**, bottom divider; hover `sidebar-accent/55`. Title **13px medium / 20px**, latest-turn preview **12px / 16px**, two-line clamp; inline expandable details.
- `ai-vault-session-row-display.tsx:34-133`: **14px agent icon**, agent name, live state dot where relevant, **N msgs**, optional subagent count, age, model; metadata **11px / 16px**.
- `SessionRowTrailingActions.tsx` and `AiVaultSessionActionMenuItems.tsx`: trailing expansion/menu and resume/open/copy/delete actions.
- The preview is the **latest conversation turn with a role label**, not invariably an assistant message: role labels include **You / Agent / Tool / System** (`ai-vault-session-row-display.tsx:171-185`).

### Agent Dashboard

- Optional left-nav entry opens **in-window non-modal companion drawer or separate pop-out**, depending on mode.
- `R/components/dashboard/AgentDashboardDrawer.tsx:35-80,157-225`: drawer starts at the **left sidebar’s right edge**, top **36px**, above the bottom status reserve; width `min(remaining viewport, 1294px)`. Sidebar stays interactive.
- Shared board: `R/components/dashboard-popout/AgentKanbanBoard.tsx:39-113,157-185`.
- Columns: **Needs You**, **Working**, **Done**, optionally **Idle**.
- Columns minimum **264px**, rounded-xl, `border-border/60`, `bg-muted/30`; headers **11px uppercase**, count pills; cards separated by **8px**.
- Search and filters select cards; most recently moved cards sort first within each bucket.
- Opening a card acknowledges its unseen state and opens a terminal-preview dialog; reveal routes back to its actual workspace/pane (`AgentKanbanBoard.tsx:216-240`, `AgentDashboardDrawer.tsx:44-55`).
- Snapshot derives pane-level agents, last user/agent messages, status, timestamps, live PTY availability and unread state (`R/components/dashboard/build-dashboard-snapshot.ts:119-180,242-256`).
- Important architectural comment: live agents were deliberately removed from a bottom-docked right dashboard and moved into workspace cards; **the right sidebar is tab-only content** (`right-sidebar/index.tsx:88-96`).

## 6. Built-in assistant finding

No separate **Orca Assistant** global page, `AssistantPanel`, or assistant-open/tab state appeared in the targeted renderer/shared searches; an alternate search of top-level view/navigation definitions likewise found no assistant entry. The concrete built-in chat implementation is **NativeChatView inside center terminal leaves**, alongside sidebar activity summaries, right-side history, and the optional live dashboard.

For Hutan, a hidden-by-default **Assistant left-nav page** is therefore a new navigation choice consistent with Orca’s global-page pattern—not a direct copy of an existing Orca Assistant region.