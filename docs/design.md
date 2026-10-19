# Hutan Design

Decisions taken with the user (2026-10-14):

| Topic | Decision |
|---|---|
| Layout | **A + B-overview + palette**: sidebar + panes + inspector by default; Overview board as a preset (⌘⇧O); command palette ⌘⇧P (⌘K in the `hutan` keymap base) |
| Configurability | **Everything is a movable component.** Any panel can live in any dock or as a center tab; every component has its own settings section; layouts are saved as presets |
| Style | **Native macOS chrome + own terminal themes**: transparent titlebar, vibrancy sidebar, SF Pro UI, system accent, light/dark follows the system; terminal and chat use Hutan themes |
| Density | Setting: `compact` (default) / `comfortable` |
| Agents with TUI and protocol | **One session, view switch Chat / Terminal** in the pane header (M4) |

Architecture of the layout system: ADR 0010.

## Principles

1. **Attention first.** The user always sees who needs them (blocked, approval, done-unread) without hunting.
2. **The terminal is sacred.** Chrome never steals keys from a focused terminal except the documented global shortcuts. Every chrome element can be hidden.
3. **Nothing is fixed.** Placement, visibility, size, and per-component behavior come from layout + settings, not code paths.
4. **Closing UI never kills work silently.** Closing a tab, pane, or window detaches. Stopping a session is an explicit, labelled action. Quitting the app asks what to do with live sessions (see § Daemon lifecycle).
5. **Native feel, calm surface.** System chrome, restrained color; color carries state, not decoration.

## Workbench model

```mermaid
flowchart TB
  W[Window] --> TB[Title bar]
  W --> LD[Left dock]
  W --> RD[Right dock]
  W --> BD[Bottom dock]
  W --> C[Center: pane tree]
  W --> SB[Status bar]
  W --> OV[Overlays: command palette, toasts, settings sheet]
  LD & RD & BD --> PS[Panel stack: one or more panels as tabs]
  C --> SPL[Split horizontal / vertical, any depth]
  SPL --> PANE[Pane: tab strip + active item]
  PANE --> IT[Items: Terminal, Chat, Diff, any Panel opened as a tab]
```

- **Panel**: a registered component with an id, title, icon, default dock, and a typed settings section. Panels: `sessions` (Projects), `tasks`, `overview` (the board of session cards, a center item of the `overview` preset), `assistant` (titled *Mawas*: Mawas Home, a tab of the center), `history`, `changes`, `inspector` (titled *Requests*), `agents`, `resources`, `notifications`, later `worktrees`, `providers`. Mawas Home is described in `docs/assistant.md` § Desktop; **Memory** (`mawas-memory`, protocol 0.32) is its sibling page: one more center tab, one instance, opened from ⌘K → *Mawas: Memory* or the Today rail.
- **Dock**: left, right, bottom. A dock hosts a stack of panels. The left dock is the **left sidebar** (its panels are *pages* chosen from a nav list), the right dock the **right sidebar** (its panels are tabs of an icon rail), the bottom dock keeps a strip of text tabs. Docks resize, can be emptied, and **collapse on demand** (§ Docks): the left sidebar is Expanded, Compact (a 44 px icon rail) or Hidden, the right side is its panel body next to a 40 px rail (open, rail only, or hidden), the bottom dock is open or hidden; collapsing never stops a session, closes a tab or dismisses an approval. Mawas Home (`assistant`) is a **page of the center**: it opens as a tab of the active pane (`WorkbenchState::open_panel_in_center`, the nav row, ⌘K, the global shortcut), never in a preset, and the user may still move it like any panel.
- **Center**: a binary split tree of panes; each pane has its own tab strip. Items are session views (terminal/chat), diffs, or any panel opened as a tab.
- **Status bar**: an ordered list of status items (`connection`, `sessions`, `attention`, `density`, `theme`, `palette_hint`, the dock buttons `dock_left`, `dock_bottom`, `dock_right`, …) left and right; each is configurable. The dock buttons sit at the right end, like Zed's: a small window glyph with the dock's side filled, drawn pressed (accent) while the dock shows. A click toggles the dock (the same action as its key), the tooltip names the key of the active keymap base, and a hidden dock without panels shows a dimmed, inert button. A preset that lists its own `status_bar.right` shows them only when it names them.
- **Title bar**: transparent with traffic lights; shows the active item title, preset switcher, Overview toggle. Can be hidden into a minimal mode.
- **Presets**: named layouts (`default`, `focus`, `overview`, user-defined). Switching a preset swaps docks/center arrangement without closing sessions.

### Moving things

| Action | Mouse | Keyboard (default) |
|---|---|---|
| Split right / down (new pane starts a new terminal in the focused terminal's cwd) | pane menu | ⌘D / ⌘⇧D |
| Move tab to split | drag tab to a pane edge (drop zones) | palette → "Move tab to new split…" |
| Move panel to another dock | drag panel tab to dock, or panel menu "Move to ▸" | palette → "Move panel…" |
| Open panel as center tab | panel menu "Open as tab" | palette |
| Resize | drag splitter / dock edge | ⌃⌘←→↑↓ |
| Focus neighbour | — | ⌘⌥←→↑↓ |
| Zoom pane | double-click tab | ⌘⇧↩ (Zed base: also ⇧⎋) |
| Toggle docks | status bar dock buttons | ⌘B left sidebar, ⌘L right sidebar (both bases; also ⌘⌥B, and ⌘R in the Zed base), Zed base ⌘J / ⌃\` bottom, ⌘⌥Y close all; Hutan base ⌘⌥J bottom |
| Switch preset / Overview | title bar menu | ⌘⇧O overview, ⌘⇧1…9 presets |
| Save current layout as preset | title bar menu | palette → "Save layout as preset…" |
| Close tab (detach) | × on tab | ⌘W |
| Stop session | session menu (confirm if running) | ⌘⇧W |

## Layout (Orca-style, 2026-10-18)

Hutan is laid out like Orca (stablyai/orca, MIT; studied in `docs/spikes/2026-10-18-orca-layout.md`): three columns, a 36 px title bar row and a 24 px status bar. Ideas ported, nothing copied.

```text
┌──────────────────────┬──────────────────────────────────┬──────────────────────────┐
│ ● ● ●                │ title bar (36 px): active item,  │ ⟲  ±  ⓘ  ⑂  ◫      strip │ 36
│                      │ Layout ▾, Overview               │ (History Changes         │
│ ⌕ Search             ├──────────────────────────────────┤  Requests Agents         │
│ ☑ Tasks              │ tab strip                        │  Resources)              │
│ ⊞ Agents        [2]  ├──────────────────────────────────┤                          │
│ ✦ Mawas (opt-in)     │                                  │  History / Changes /     │
│ Projects      ＋ ⇅ ▶ │  terminals · chats · diffs       │  Requests / Agents /     │
│ ▸ hutan              │  (split panes)                   │  Resources body          │
│   ◆ M2 · claude      │                                  │                          │
│   ❯ tests            │                                  │                          │
│ ＋ New session  ⚙    │                                  │                          │
├──────────────────────┴──────────────────────────────────┴──────────────────────────┤
│ hutand connected · 7 sessions · ● 2 need you                 Compact · Dark · ⌘⇧P  │ 24
└────────────────────────────────────────────────────────────────────────────────────┘
   left sidebar 280 px                      center                  right sidebar 350 px
```

**Left sidebar** (280 px expanded, resizable 220–500 or what leaves the center 480 px; ⌘B; 44 px compact, see § Docks). From the top: the inset under the traffic lights, the **nav list**, the **page**, a bottom toolbar.

- Nav rows (Orca's metrics: 8 px outer padding, 2 px gap, rows padded 8 × 6 px around a 16 px icon and an 18 px line of 13 px medium label — **30 px rows**; muted icon and label when inactive, `fg` and a `sidebar_accent` fill when selected, an 8 % `fg` fill on hover): **Search** (opens the jump palette, the ⌘P / ⌘J "Jump to…"), **Tasks** (the Tasks panel as a page), **Agents** (opens the Agent Dashboard drawer, below; a pill counts the sessions that need you), **Mawas** (only while `assistant.enabled`; opens Mawas Home as a center tab, selected while that tab shows; see below), and a row for every other panel the user keeps in the left dock. A page row toggles its page; right-click gives the panel menu. Choosing another row closes the drawer.
- The default page is **Projects** (`sessions`): a 32 px section header (12 px semibold, muted, with Start, +, the sort/organize menu and ⚙), the filter, and the project / workspace / session tree drawn as cards (13 px titles, a 20 px status lane with the state dot and, for an unread session, an amber dot ringed in the sidebar color; branch / checkout badge; live-agent badge; hover and selected fills from `sidebar_accent`). Any other page replaces the list and shows a **‹ Projects** back row.
- Bottom toolbar: **New session** (⌘T) and **Settings**, with icons.

**Agent Dashboard drawer.** The *Agents* nav row (or ⌘K → *View: Toggle Agent Dashboard*) opens a non-modal drawer over the center, from the left sidebar's right edge, below the 36 px title bar and above the status bar, `min(remaining window width, 1294 px)` wide. The left sidebar keeps its Projects list. A kanban of the sessions (assistant chats excluded): **Needs you** (blocked on an approval or a question, failed and unseen, waiting for review), **Working**, **Done** (ended, or finished and not looked at yet) and **Idle**; columns at least 264 px wide (the board scrolls sideways below that), an 11 px uppercase header with a count pill, cards 8 px apart showing the session title, the agent, its last message (two lines, cut with "…") and the time since its last report. A card opens or focuses its session and closes the drawer; **Escape** or the nav row again closes it too (the drawer holds the keyboard while it is open, so Escape reaches it wherever the focus was). The `overview` board in the center (⌘⇧O) is unchanged.

**Right sidebar** (a 350 px body, 240–480, next to a 40 px **activity rail** outside it; ⌘L). The rail runs from the title bar row to the status bar, always there unless the user hid it, with 32 px icon targets and tooltips: **History** (default, the Agent Session History), **Changes** (the checkouts of the workspace, uncommitted ones first; a click opens the full Changes view as a tab), **Requests** (the inspector: approvals and block requests; its badge keeps the number of waiting requests, also while the body is collapsed), **Agents** (the observation tree / monitor), **Resources**. Icons that do not fit the height move into a **⋯** More menu (the active one always stays visible). Icons drag to reorder or to another dock. The body has a 36 px header with the panel's name. See § Docks for what the icons and ⌘L do.

**Mawas.** `assistant.enabled` is **off** by default for a new settings file (a user whose layout was saved by Hutan before this layout keeps it on: `assistant.enabled = true` is written once). When on, *Mawas* appears in the left nav and **Mawas Home** opens as **one tab of the center** (a registered panel like the others, so it can be closed, reopened and moved; no preset places it): ⌘K → *Mawas: Open*, *Mawas: New chat*, *Panel: Open Mawas*, the nav row, the menu bar item and the global shortcut all show it (`OpenAssistant` needs the setting on); turning the setting off closes it. The page is a **header** (scope dropdown, state dot, backend·model chip, **mode chip** *Ask before acting ▾* / *Advisor ▾*, **New chat**, **Stop**, **Today** toggle), the conversation (the ordinary chat view, centered, at most 46 rem wide) and the collapsible **Today** rail (300 px, left border): *Needs you*, *Working*, *Upcoming* ("No schedules yet"); no link to a service that does not exist. Mawas's own approvals are cards above the conversation and the first rows of *Needs you*. A page that is not the active tab does no work (no re-render on store changes, no timer). Text is cut with "…" at the room it has; `docs/assistant.md` § Desktop has the behavior.

**Icons.** The nav, the right strip (History `history`, Changes `git-compare`, Requests `info`, Agents `git-branch`, Resources `gauge`, More `ellipsis`), the History header and rows (`rows-3`, `list-filter`, `refresh-cw`, `settings`, `chevron-down` / `chevron-up`, `ellipsis`), the Projects header (`plus`, `arrow-up-down`, `settings`), the Changes refresh and the bottom toolbar (`plus`, `settings`) draw **Lucide** SVGs (ISC; `assets/icons/`, license in `assets/icons/LICENSE-lucide.txt`, only the files used). They are embedded in the executable at compile time (`crates/desktop/src/icons.rs`, an `AssetSource` installed with `Application::with_assets`), so the app bundle needs no resource files, and drawn 14–16 px with `svg()` tinted by a theme color (`muted`, `fg` or `accent`). A panel without an icon of the set keeps its single glyph. Adding an icon is one more SVG and one more line in the `icons!` table; a test iterates the table and checks that every path is served.

**No clipped text in the sidebars.** A name, a chip or a button in either sidebar is cut with "…" at the room it has (Projects tree titles and subtitles, the Tasks chips and detail lines, the Changes rows, the Requests rows and tabs, the Mawas header, the History titles and group labels, the nav labels and the toolbar); a row of chips wraps instead of overflowing. A pill (`ui::pill`) is at most 140 px wide.

**Window title.** The title bar and the window title name the active item of the **center** (a session, a tab); the page a sidebar shows never becomes the title, so focusing History or Tasks leaves it as it was (Mawas Home, a center tab, is the title while it shows).

**Tokens.** Chrome follows Orca's structure in Hutan's warm hues (see § Visual system): the right sidebar has its own surface (`right_sidebar`, darker than the left in dark, lighter in light), selected rows step away from the sidebar (`sidebar_accent`, `right_sidebar_accent`), the dark hairline is white at 7 %, the title bar and the right strip are 36 px, the status bar 24 px in both densities. Accent (orange) and fonts are unchanged.

### Docks: collapse on demand, always recoverable (DK-1)

Design: `docs/spikes/2026-10-20-chrome-v3.md` § 4 and its mockup. What the user chose lives in the saved layout; the window may show less for a while (below), never more.

| Dock | Expanded | Compact | Hidden |
|---|---|---|---|
| **Left sidebar** | 280 px (220–500), the nav, the page, the toolbar | a **44 px icon rail**: Search, Tasks, Agents, Mawas (when enabled), any other page, Projects; New session and Settings at the bottom; 32 px targets, a tooltip on every icon, the Agents count stays as a badge. Search, Agents and Mawas work as they do expanded and do not widen the rail; Tasks and Projects are pages of the sidebar and expand it | 0 px, no hitboxes, no focus stop |
| **Right side** | the panel body (350 px, 240–480) next to the 40 px rail | the **rail alone** (History, Changes, Requests, Agents, Resources, ⋯) | no rail (explicit *Hide Right Rail*) |
| **Bottom** | the body with its tab strip | — (it has no rail) | 0 px |

- **⌘B** toggles the left sidebar between Expanded and Compact; from Hidden it comes back in the mode it was hidden from. **⌘L** (the rail's bottom button, the titlebar's right-dock button) toggles the right body between open and rail only; from Hidden it opens. **⌘⌥J** / **⌘J** (per keymap base) toggles the bottom dock, tab strip included. The status bar's dock buttons dispatch the same toggles. No existing default key changed.
- **Always recoverable.** A **titlebar button** sits next to the traffic lights for the left sidebar (after the 80 px they take when the sidebar does not cover them) and at the right end for the right side. The left one hides the sidebar, or brings it back; the right one does what ⌘L does. A right click on either lists the states (*Expanded* · *Compact (icons)* · *Hidden*; *Panel open* · *Rail only* · *Hidden*) with the current one checked. *View: Hide Left Sidebar* and *View: Hide Right Rail* (palette) hide completely; the titlebar buttons stay.
- **The right rail.** Clicking the icon of the panel the body shows collapses the body to the rail; the rail stays and remembers the panel and the width. Clicking any icon while collapsed opens that panel, the remembered one included; clicking another icon while open switches panel. The icon of the remembered panel of a collapsed body looks selected, not pressed. A new permission request never expands a body the user collapsed and never takes the keyboard: the Requests icon is selected, keeps its badge, and a notice says a request is waiting.
- **Drag to collapse.** The edge of an expanded side dock tracks the pointer down to its minimum (220 / 240 px) and stops. 24 px further, a translucent **preview** of the rail appears at the window edge; releasing the mouse collapses the dock to its rail, Escape cancels (the dock keeps the size it had, nothing collapses on release). A collapsed dock reopens at the width it had before the drag; nothing in between is saved. The compact rail has no edge to drag. The dock edge also stops where the center would get narrower than 480 px.
- **Responsive collapse** (`layout.auto_collapse`, default on; Settings → Layout *Collapse sidebars in a narrow window*). Below **1180 px** of window width the right body collapses to its rail; below **900 px** the left sidebar goes Compact; the center keeps at least **480 px** (a dock the center cannot afford collapses at any width, the right one first). A collapsed dock comes back only **80 px** above its threshold, and only when the center fits with it open. This is temporary: it is not saved and never overwrites what the user chose, a dock the user hid or collapsed is never auto-restored, and a dock the user opens in a narrow window (⌘B, a rail icon, a command) stays open until the window has grown past the same margin. At 1280 / 1180 / 900 / 640 px: both open · both open · right on its rail · both on their rails.
- **Focus.** A dock that stops being drawn as a body (user or window) hands the keyboard to the last active center item; a hidden element never keeps focus or a hitbox. Collapsing never stops a session, closes a tab or dismisses an approval.
- **Saved** per layout (`desktop-state.json` version 5, see *Layout migration*): each dock's `visible` and `collapsed`, its width and active panel, per saved workspace layout; and per layout preset the modes and widths the user left, which come back when the preset is applied again (applying the current preset again starts over from its own docks). The Agent Dashboard drawer works as before; it is not saved.
- **Icons.** The titlebar buttons and the rail's collapse button use Lucide `panel-left` / `panel-right`, the compact rail's Projects icon `folder` (`assets/icons/`, the set's ISC license), tinted like the other icons.

### Presets shipped

| Preset | Left sidebar pages | Center | Right sidebar tabs | Bottom dock | Use |
|---|---|---|---|---|---|
| `default` | sessions (Projects), tasks | pane tree | history, changes, inspector, agents, resources | empty, hidden | daily work |
| `focus` | same, hidden | pane tree, zoomed active pane | same, hidden | empty, hidden | deep terminal work (layout C) |
| `overview` | sessions, tasks | `overview` panel as the only center item | history, changes, inspector, agents, resources | empty, hidden | many agents at once (layout B) |

Mawas Home is in no preset; it is a center tab, so a preset that replaces the center takes it with the rest of the center.

### Layout migration

The saved layout (`desktop-state.json`) is version 5. Version 5 (DK-1) gave the side docks a rail: a dock now has `collapsed` next to `visible` (visible and collapsed is the 44 pt left icon rail, or the right activity rail alone; hidden keeps the mode the dock comes back in), the layout keeps what the user left the docks at in each preset (`dock_memory`, per saved layout and per parked workspace layout), and the side widths are clamped to 220–500 (left) and 240–480 (right body). A version 4 file needs no rewrite: `visible: true` is Expanded and `false` is Hidden (the choice is kept), `collapsed` defaults to off, panels and widths stay, and the widths are clamped like every loaded layout; the next save writes version 5. Version 4 (MW-1) turned the Assistant from a left page into Mawas Home, a center tab: a version 3 file (and an older one, migrated first) loses the `assistant` panel from every dock, once, in the live layout, the overview stash and every parked workspace layout; a Home tab already in the center stays. Version 3 (OL-4) turned *Agents* from a left page into the dashboard drawer: a version 2 file (and a migrated version 1 file) loses the `overview` panel from its left dock, once, in the live layout, the overview stash and every parked workspace layout; an `overview` the user put in another dock or a center tab stays. A version 1 file is migrated once when it is read, for the live layout, the overview stash and every workspace's parked layout: History moves to the front of the right dock and becomes its active tab; Changes joins the right dock after it; Resources moves there from a hidden bottom dock; Tasks moves from the right dock to the left sidebar behind Projects; the Assistant leaves every dock and the center (its conversations stay in the daemon; the nav opens it again); Projects becomes the left page; a dock still at the old default width (240 / 300 px) takes the new one (280 / 350 px). Center tabs, splits, zoom, focus, the preset name, user-changed widths and a visible bottom dock are kept.

## Closing and reopening sessions (CL-1)

Every session row of the sidebar (project tree, Home, Needs you) and every card of the Agent Dashboard has a hover **×**; "Close session…" is also in the row's, the card's and the tab's context menus and in the palette. `session.close` removes the session from the lists but keeps its journal and history. A session nothing runs in closes at once and a toast says "Closed <title> · Undo" for 5 s (Undo = `session.reopen`); when something runs (a foreground command, a working agent, a chat turn) the daemon refuses and a dialog names it — "Close <title>?", **Stop and close** / Cancel (Escape cancels; focus returns to what had it). Closing removes the session's tabs and selects its neighbour in the project. A terminal tab's × still only detaches the tab; `terminal.tab_close = "close_session"` makes it close the session. History shows **Reopen** for closed Hutan sessions; older daemons without `session.close` show no ×.

## Session pane header

`[kind icon] title  [state chip]  [placement badge]  [Chat | Terminal]  … ⋯`

- Kind icon: `❯` terminal, `◆` coding agent, `✦` direct model chat.
- State chip: see attention model. Hidden for plain idle shells unless enabled.
- Placement badge: `main` (main checkout) or `wt <branch>` (worktree); click → reveal in Finder / copy path.
- View switch: only when the session has both a TUI and a structured protocol (M4).
- `⋯` menu: rename, restart, stop, move to split/dock, copy session id, open in new window.

## Attention model

| State | Color token | Indicator | Meaning |
|---|---|---|---|
| `blocked` / approval | `--warning` (system orange) | filled dot, pulses once on entry | needs a decision |
| `done` (unread) | `--success` (system green) | filled dot until viewed | finished, not looked at |
| `working` | `--accent` (system accent) | dot with subtle activity ring | running a turn / output active |
| `idle` | `--muted` | hollow dot (optional) | alive, waiting |
| `failed` / exited ≠ 0 | `--error` (system red) | dot + code in subtitle | needs a look |

Rollups: session → workspace header → sidebar "Needs you" → status bar `attention` item → menu bar item and Dock badge (M6-C, § macOS integration) → notification (M6-B).

## Visual system (v2, 2026-10-15)

**Style, not layout.** Hutan stays terminal-first with its workbench (docks, panes, presets). What changes is how everything looks: calm, crisp and warm, in the spirit of Claude Desktop — generous spacing, soft borders instead of boxes, one clear accent, readable prose. Brand colors come from `assets/brand/README.md` (orangutan orange `#E0692E`, gold `#F5B53D`).

- **Typography**
  - UI: system sans (SF Pro), 13 px body / 12 px secondary (comfortable) and 12 / 11 (compact); weights regular and medium only; section labels 11 px medium, sentence case (no all-caps, no letter-spacing tricks).
  - Prose (assistant/agent messages, markdown in chat, notification and empty-state copy): system serif **New York** (fallback Georgia), 15 px, line height 1.6; headings in prose use the sans at medium weight.
  - Code and terminals: monospace (`terminal.font_family`, default Menlo; JetBrains Mono / SF Mono when installed), 13 px.
  - Numbers in counters and durations use tabular figures.
- **Palette** (the built-in packs `hutan-light` / `hutan-dark`; the first column is the field of `Theme`, each is a token of `docs/themes.md`, `bg` is `ui.background`, `fg` is `ui.foreground`, `danger` is `state.error`, and so on):

| Token | Light | Dark | Use |
|---|---|---|---|
| `bg` | `#FAF9F5` | `#262624` | window and center panes |
| `sidebar` | `#F3F1EA` | `#1F1E1D` | left sidebar, titlebar strip, status bar |
| `sidebar_accent` | `#E9E6DC` | `#2E2D2B` | selected nav row and active project card of the left sidebar (Orca's `worktree-sidebar-accent`) |
| `right_sidebar` | `#F8F7F2` | `#1A1918` | the right sidebar and its strip (Orca's `sidebar`) |
| `right_sidebar_accent` | `#F0EEE6` | `#2A2927` | active strip icon and hover rows of the right sidebar |
| `surface` | `#FFFFFF` | `#30302E` | cards, composer, popovers, palette |
| `surface_hover` | `#F0EEE6` | `#3A3936` | row and button hover |
| `border` | `#E6E3D9` | white at 7 % | hairlines (1 px; dark follows Orca's `--border`), focus rings use `accent` |
| `fg` | `#1F1E1D` | `#F5F4EF` | primary text |
| `muted` | `#6F6D66` | `#A6A49C` | secondary text, icons |
| `faint` | `#A8A59C` | `#6F6D66` | placeholders, disabled |
| `accent` | `#E0692E` | `#EA7A43` | primary buttons, selection, focus, links |
| `accent_soft` | `#FBE9DF` | `#3E2C23` | selected row, chips, user message bubble |
| `success` / `warning` / `danger` | `#3F8F5A` / `#C98A12` / `#C8452F` | `#6BBF85` / `#E2AE45` / `#E87562` | states |

  The terminal section of a pack has its own palette (`terminal.*` tokens); in the built-ins its background matches `bg` so panes do not look like holes. Nothing in the desktop or the terminal view holds a color literal: every value above lives in `assets/themes/<id>/theme.toml` and reaches the views as a resolved token (`docs/themes.md`). The dark hairline is `#ffffff12` (alpha 18/255 = 0.0706) and the dark and light shadow and scrim are 8-bit alphas of black (0.40, 0.12 and 0.50, 0.28 become 0.40, 0.1216 and 0.5020, 0.2784): the only places where the packs are not bit-for-bit the old literals.
- **Shape and depth**: radius 8 px (controls), 12 px (cards, composer, popovers, palette), 999 px (pills, badges); no drop shadows inside the window except popovers/palette (one soft shadow, 0 8 24 rgba(0,0,0,.12) light / .4 dark); separators are 1 px `border` hairlines, used sparingly.
- **Components**
  - *Sidebar rows* (the Projects tree; the left nav uses Orca's 8 × 6 px rows, 13 px medium): 30 px (comfortable), 8 px horizontal padding, rounded 8 px hover/selected (`surface_hover` / `accent_soft`), icon 16 px `muted`, title `fg`, second line or right side `muted`; counters as small pills.
  - *Buttons*: primary = `accent` fill, white text; secondary = `surface` with `border`; ghost = text only with hover fill; all 28 px high, 8 px radius.
  - *Chat*: no bubbles for agent messages (prose in serif on `bg`, max width 720 px centered in the pane); user messages in an `accent_soft` rounded bubble aligned right; tool calls as quiet collapsible cards (`surface`, 12 px radius, mono summary line, status dot); approvals as a card with the offered options as buttons, the first option primary.
  - *Composer*: a `surface` card with 12 px radius and hairline border at the bottom of the chat, multi-line field, a row below with attach (+), context chips, model/harness picker, and a round send button (`accent`) that turns into stop while streaming.
  - *Palette, popovers, menus*: `surface`, 12 px radius, soft shadow, 6 px inner padding, rows like sidebar rows.
  - *Empty states*: the mascot state image (`assets/brand/05-mascot`) at 96 px, one serif sentence, one action button.
- **Density tokens**:

| Token | compact | comfortable |
|---|---|---|
| sidebar row | 26 px | 30 px |
| tab height | 30 px | 34 px |
| pane header | 30 px | 36 px |
| status bar | 24 px | 24 px (the old 22 / 26 table in `hutan-settings` was never drawn and is gone) |
| base spacing | 4 px | 6 px |
| control radius | 6 px | 8 px |
| card radius | 10 px | 12 px |
| terminal padding (all sides) | 8 px | 12 px |
| block gutter bar | 3 px | 4 px |

- **Chrome**: transparent titlebar with inset traffic lights over the `sidebar` color; light/dark follows macOS unless pinned (`appearance = "system" | "light" | "dark"`). The accent is the brand orange, not the system accent.
- **Themes** (ADR 0018, `docs/themes.md`): a color pack is a data-only TOML file (`theme.toml`) that drives chrome tokens, the terminal palette, state and diff roles, fonts, density and effects, and (M11) syntax scopes; the built-in `hutan-light` and `hutan-dark` are packs parsed by the same loader as the user's own in `<data dir>/themes/`. `appearance.appearance` keeps choosing light or dark; `appearance.light_theme` / `appearance.dark_theme` choose the pack of each. The desktop reports the active terminal colors to the daemon so OSC 10/11 replies match what the user sees, once per committed theme change.
- **Motion**: ≤ 150 ms ease-out for hover, popovers and expanding cards; no motion on terminal content; respects Reduce Motion.

## Settings

- Files (TOML, comments allowed):
  - user: `~/Library/Application Support/Hutan/settings.toml`
  - keymap: `~/Library/Application Support/Hutan/keymap.toml`
  - workspace overrides: `<first project root>/.hutan/settings.toml` of the workspace shown (Home: its folder)
  - project overrides: `<project root>/.hutan/settings.toml` of the project you work in (only keys marked workspace-overridable, in both files)
- Precedence: built-in defaults < user < workspace < project < runtime toggles (e.g. density switch writes back to user file). A project outside any workspace layers only its own file; the project layer is left out when it would read the file the workspace layer reads.
- Every component owns one namespace with a typed, documented schema; unknown keys warn, invalid values fall back to defaults with a visible error in the settings UI. Files hot-reload.
- Settings UI (⌘,): generated from the schemas, searchable; each panel's gear (⚙) opens its own section. It is where Hutan is managed, not only configured: see *Settings sheet* below for layers, Reset, validated choices, and the pages that manage the history index, the agents' hooks, remembered choices, the data folder and the keymap.
- `terminal` namespace, session start (B-1): `shell` (string; empty = the account's login shell) and `env` (table of strings, extra variables) are overridable per workspace and per project; `terminal.env` of the workspace's first project and then of the project overrides the user's per variable. They are resolved for the project a new terminal starts in (which need not be the one on screen; a project outside any workspace: its own file alone) and sent in `session.create` as `TerminalSpec.env` (`shell` as `SHELL`). Values are passed to the daemon and never logged or shown. Where the rest of the environment comes from: `daemon.toml` `[terminal] env_inherit` (`docs/architecture.md` § Session environment).
- Layout geometry the user drags (sizes, open tabs, split ratios) is **state**, saved in `desktop-state.json`; it becomes a **preset** in settings only when the user saves it.

```toml
[app]
keep_running = false          # leave the daemon running after Hutan quits (no login item)
start_at_login = false        # start the daemon at login: registers the launch agent (Hutan.app only)
setup_done = false            # the first-run setup was completed or skipped
on_last_window_close = "quit" # quit | menu_bar (keep Hutan in the menu bar; needs menu_bar = true)
menu_bar = true               # menu bar item: daemon state, count, sessions that need you
dock_badge = true             # number of sessions that need you on the Dock icon

[assistant]
global_shortcut = ""          # system-wide shortcut that opens Mawas, e.g. "alt-space"; "" (default) is off

[keymap]
base = "zed"                  # zed | hutan: the default key table (keymap.toml is merged on top)

[appearance]
density = "compact"            # compact | comfortable
appearance = "system"          # system | light | dark (pre-v2 files call this key `chrome`; still read)
light_theme = "hutan-light"    # color pack of light windows (docs/themes.md)
dark_theme = "hutan-dark"      # color pack of dark windows

[terminal]
font_family = "Menlo"
font_size = 13
theme = "auto"                 # auto (the active pack's terminal colors) | hutan-dark | hutan-light | <pack id>
cursor = { style = "block", blink = true }
option_as_meta = "left"        # none | left | right | both
copy_on_select = false
blocks = true                  # prompt blocks over the terminal (see Terminal blocks)

[layout]
preset = "default"
auto_collapse = true           # a narrow window shows the right panel as its rail below 1180 pt, the left sidebar as its icons below 900 pt (Layout § Docks)

[layout.presets.default]
left   = { panels = ["sessions"],  size = 240, visible = true }
right  = { panels = ["inspector", "agents"], size = 300, visible = true }
bottom = { panels = ["resources"], size = 200, visible = false }
status_bar = { left = ["connection", "workspace", "sessions", "attention"], right = ["density", "theme", "palette_hint", "dock_left", "dock_bottom", "dock_right"] }

[sidebar]
organize = "workspaces"        # workspaces | projects (see Projects and workspaces)

[panels.sessions]
group_by = "workspace"         # workspace | state | none
needs_you = true
row = ["state", "kind", "title", "folder", "placement", "age"]   # folder + placement are the row's second line

[panels.inspector]
tabs = ["approvals", "changes", "info"]
reveal_on_approval = true

[panels.overview]
card_size = "medium"           # small | medium | large
preview = "live"               # live | static | off
sort = "attention"             # attention | recent | name

[workspaces]
sort = "manual"                # manual | recent | attention | name (applies within each level)
hide_empty = true              # hide projects without sessions
hide_sleeping = false          # hide projects without a live session
hide_cli_created = false       # hide projects the CLI or an agent created
always_show_active = true      # the project and workspace you are in stay listed whatever the filters say

[history]
sources = []                   # default source chips: hutan | claude | codex | omp (empty = all)
roles = []                     # default role chips: user | assistant | tool | command | output
range = "all"                  # 24h | 7d | 30d | all
project = "all"                # workspace | all
show_snippets = true

[agents]
show_finished = false          # list finished agents (dimmed, their subtree folded into one row)
only_blocked = false           # list only blocked agents (and the ones above them)
scope = "active_workspace"     # active_workspace | all

[editor]
vim_mode = false               # Vim's Normal / Insert / Visual in text inputs (see Vim mode); ⌘K → Toggle Vim Mode
vim_use_system_clipboard = true  # yanks and deletes also go to the system clipboard; p pastes from it
```

### Settings sheet (ST-2)

Every control shows the truth and can be undone.

- **Layers and provenance.** Under each field's help a line says where the effective value comes from: *Default*, *You* (`settings.toml`), *Workspace NAME* or *Project NAME* (the `.hutan/settings.toml` of the container shown / the project worked in); the file is the hover. A control edits the layer its value comes from when that is a workspace or project file, otherwise the user's file (a namespace a workspace may not override is always the user's), so a value never seems not to take. When a workspace or project layer exists for the container and the page has overridable namespaces, the header offers **Edit in: You · Workspace · Project** (only the layers that exist); a choice moves every control of the page to that layer, and the provenance line then says `· editing <layer>` where the value comes from another one. `SettingsStore::layers`, `source_of`, `layer_value`, `write(layer, …)` and `remove(layer, namespace, key)` are the store side.
- **Reset removes, it does not write.** A field's *Reset* (shown while the file it edits sets the key) removes the key from that file, so the value falls back to the layer below, or to the default; comments elsewhere stay and a table the removal empties goes unless a comment is attached to it. *Reset page…* removes every key of the page from the file it is edited in, after a confirmation that lists them with their layer. Saved layout presets (they have their own Delete) and the two fields that mirror the daemon's lifetime and the login item (`app.keep_running`, `app.start_at_login`) are left out.
- **Validated choices and ranges.** Where the app knows the set, the sheet offers it: `terminal.theme` (Automatic and the installed themes), `layout.preset` (the built-in presets and the ones saved), `panels.sessions.row` (the six row parts) and `panels.inspector.tabs` (approvals, changes, info) are picked from a menu, each once; a value in the file that the app does not offer is shown as *(not available)*. Numbers show their range and are clamped when typed, with a notice: `terminal.font_size` 4–72, `terminal.line_height` 0.8–3, `resources.interval_ms` 1000–60000. `hutan ui set` refuses what the sheet would not offer.
- **Maps.** `terminal.env` is a list of `NAME` = value rows: add with `NAME=value`, click a value to edit it, × removes the row from the layer being edited; it warns that the values stay in `settings.toml` in plain text. `layout.presets` lists the built-in and the saved presets with *Rename…* and *Delete…* (the preset in use cannot be deleted; deleting one that replaces a built-in restores the built-in) and *Save current layout as…*, which also makes the saved preset `layout.preset`.
- **Pages the app draws itself.** *History*: an On/Off for indexing (`history.set_enabled`) with the sources, rows and size from `history.status`; the History panel's scan strip opens **Disable** next to Clear and Rebuild (disabling keeps `index.sqlite`). *Agent Integrations*: per agent (OMP, Claude Code, Codex) the hook status — installed, outdated or incomplete, not installed — with *Install hooks…*, *Repair hooks…* and *Uninstall hooks…* (`hutan integration`), each confirming with the files it changes; `auto_install` stays, and the page says that turning it off does not uninstall. *Start Sheet*: the remembered “Don't ask again” agent/model choices with *Revoke* and *Revoke all*; turning *Ask before an agent chat sends its prompt* on while some are remembered asks whether to forget them. *Models*: *Clear key* asks first. *Data & logs* (after General): the data folder, `settings.toml`, `keymap.toml`, `daemon.toml` and the logs folder with *Reveal in Finder* and *Open* (Hutan's own `settings.toml` and `keymap.toml` are created with a comment when missing; `daemon.toml` is the daemon's and is never created), the size of `hutan.db` and of the history index, the daemon's version, pid and uptime, and *Restart daemon…* / *Stop daemon…*, which are the Daemon menu's flows with their confirmations. *Keyboard*: the bindings of `keymap.toml` (keys, action, context) read-only, with *Open keymap.toml* and *Reload*; the search finds the page by a key or an action and narrows the list. A full binding editor is not part of the sheet.
- **Search** matches a page's title, key, fields (title, help, choice labels), the words of its custom controls (Models and Accounts: API key, endpoint, base URL, model, provider, login, logout, account; Data & logs: Reveal in Finder, restart daemon, …) and the bindings of the Keyboard page.
- **Catalog.** `settings_schema::catalog()` is the list of pages in order; an entry names its `page` when it is a section of another page (`sidebar` shows on *Projects Sidebar*), and pages without a namespace (`data`, `accounts`, `models`) are drawn by the app. A page for the daemon's own settings is added there.

### Daemon settings in the sheet (ST-3)

The daemon's own options (`daemon.toml`, protocol 0.28, capability `daemon.settings`) are managed in the same sheet, drawn from `daemon.settings.get` with the generic controls (`workbench/settings_daemon.rs`): a toggle, a range-checked number box, a choice, a path with **Choose…** (the system's open panel: a file for `bin` / `cli`, a folder otherwise) and **Use default**, a list of text (chips; the order of arguments counts, so they have ↑ ↓) or of choices (a menu of the ones not in the list), a table of variables (`NAME=value`, with the warning that daemon.toml is plain text), and the MCP servers (`workbench/settings_mcp.rs`): a row per server with **Edit** and **Remove…** (which asks first) and **Add server…**, a form with the name, *A program on this Mac* (program and arguments, one line with quotes for arguments that hold spaces) or *A remote server* (address and HTTP/SSE); a missing field or the daemon's refusal shows inside the form, a saved server closes it, and the change counts after the daemon restarts (the sheet's banner) Nothing is cached: a change is sent with `daemon.settings.set` and the answer replaces the list.

- **Pages.** *Notifications* (`notify.*`: master switch, system notifications, which events, shortest turn, expiry; there is no test notification in the daemon, so no such button), *Sessions & restore* (`daemon.restore_sessions`, `workspaces.home_folder`, `terminal.env_inherit`, `projects.auto`), *Agent control & security* (`control.*`: *Ask me / Allow / Read only* with a plain explanation of what an agent may do to Hutan), *Agents & tools* (OMP, Claude Code, Codex programs, arguments, environment and CLI; the MCP servers, added, edited and removed in the page; the gateway's conversation length; parallel tasks), and sections of two existing pages: *History* (the sources, how far back, the transcript folders, under ST-2's On/Off) and *Assistant* (below). Each key has plain wording in `settings_labels.rs`; the permissions say what they let the assistant read.
- **Default or set, and when it applies.** Under each field: *Default: X* or *Set in daemon.toml (default: X)*, then *applies at once* (live) or *applies after the daemon restarts*; once a restart field was changed the line says *saved, applies after the daemon restarts*. **Reset** (**Use default** for a path) removes the key from daemon.toml.
- **Restart banner.** While any option's file value is not what the running daemon uses (`restart_pending`), every page of the sheet starts with *Some changes apply after the daemon restarts.* and **Restart now**, which is the daemon restart flow (its confirmation while sessions are live, which says what the new daemon will do by `daemon.restore_sessions`: *restores them* for respawn, *lists them stopped (your setting “Leave them stopped”); start each again from its Restart action* for stopped, and no promise while the setting is not known yet; sessions are restored by `daemon.restore_sessions`). The banner goes when the new daemon answers.
- **Errors.** A refusal of the daemon (validation: range, choice, a path that is not there) shows in the daemon's words under its field, and the number or text that was refused stays in its box (red) until it is corrected or given up with Escape; the range line under a number says the real rule where the daemon's minimum and maximum do not (*0 (never) or at least 60* for the assistant's idle sleep). The footer's **Problems · N** counts these refusals, a `daemon.toml` that cannot be read and the provider error of the Models page next to the problems of the settings files, and lists them. A `daemon.toml` the daemon cannot read (`invalid_state`, with file, line and column) is a banner on the page with **Open daemon.toml** and **Check again**. An agent-originated connection is always refused by the daemon (ADR 0016); the desktop is never one. A daemon without the capability: the pages say *Update the daemon to manage these settings here.*, with the path of the file and **Open daemon.toml**.
- **Variables and provider errors.** The warning under a table of variables names the file that keeps them: *daemon.toml* for the daemon's (harness environments), *settings.toml* for `terminal.env`. A provider that fails (the Models page, the Start sheet, the model picker of a direct chat) is explained by the kind of failure — could not connect (*Could not connect to r4-local at http://127.0.0.1:9. Check that the server is running and the address is right.*), no answer in time, the key refused or missing (401 / 403), model or address not found (404), anything else — with what the daemon said (attempts, the connection error) behind a **Details** toggle; the mapping is `provider_failure` in `workbench/providers.rs`.
- **Search** covers the wording of the daemon fields and their keys.
- **Mawas page** (`assistant`). Under the app's own fields (*Use Mawas*, shortcut): **What Mawas may do** (*Mode*: *Ask before acting* / *Advisor*; *Changes per turn before every one asks*), **What you allowed Mawas to do without asking** (the grants: tool, where it applies — a chat, or always in a workspace — and **Revoke**; **Revoke all**; destructive changes always ask whatever is granted), then **Backend and model** for *Global (Home)* and for the workspace or project shown. Each has **Inherit** (`assistant.reset`; the line under it says what it inherits and from where: the parent workspace, then Global, then daemon.toml, then Automatic), **OMP · Claude Code · Codex · Provider** (a provider is then picked from the list) and a **Model** (*Automatic*, which stores no model, or one of the backend's catalog). A line says what the default Mawas of the place runs on now (`assistant.list`); when nothing was chosen it begins *Automatic · backend · model*, the same words as the page's header. Then the file's own *Backend / Provider / Model when nothing is chosen*, **what Mawas may read in projects** (`assistant.context.*`), **what Mawas may read at Home** (`assistant.global_context.*`, files off by default), **what Mawas may read in this workspace** (the overrides `assistant.workspaces.<id>.*`: *Inherit · Allow · Deny*, Inherit removes the key) and the attachment size. The daemon has no read of the stored per-place choice, so the sheet shows what it stored or reset since the window started; a choice made before is not known (the *runs on* line is the truth).
- **Keyboard.** **Tab** and **Shift-Tab** move between the search box, the list of pages and the controls of the page. In the list **↑ ↓ Home End** select pages; in the controls **↑ ↓** move between rows and **Return** or **Space** acts (turns a toggle over, opens a choice, starts a text box, asks for a path). An open choice (desktop and daemon pages alike, however it was opened) is moved through with **↑ ↓**, **Return** picks the option and **Escape** closes it without changing the value; only a second Escape closes the sheet. Typing anywhere goes back to the search box. A click inside the sheet puts the keyboard back in it, and a confirmation that opens over it (*Clear key*, *Install / Repair / Uninstall hooks*, *Reset page*, *Revoke all*, *Stop / Restart daemon*, *Remove provider*, …) hands the keyboard back to what had it — the search box or the control — whether it was confirmed or cancelled (`modal.rs` remembers the focus when a dialog opens and restores it when it closes; with nothing to restore it goes to the sheet while it is open, else to the focused pane).
- **Empty search.** A search that matches no page shows *No settings match “…”* in place of a page, with a hint and **Clear search**; the footer says *No matches* rather than *Problems · none*.

### Settings wording

The sheet speaks to users, not to people editing `settings.toml`: the schemas' doc comments name keys, types and files, so the sheet shows the wording of `crates/desktop/src/workbench/settings_labels.rs` instead (which this section mirrors; a test fails for any field or choice of a built-in namespace that has no entry there, and for any title, help or choice label that contains a code span, `::` or a snake_case key name).

- **Namespaces** are listed by title: Appearance, General (`app`), Data & logs, Keyboard (`keymap`), Terminal, Layout, Projects Sidebar (`panels.sessions`, with `sidebar` as *Organize the sidebar*), Requests (`panels.inspector`), Overview, Workspaces, History, Chat, Editing (`editor`), Agents, Resources, Tasks, Mawas (`assistant`), Agent Integrations (`integrations`), Start Sheet (`launcher`), Accounts, Models. The key stays what `settings.toml` and `hutan ui set` use, and a search finds a namespace or field by its key as well as by its title, help or the label of a choice.
- **A field** has a title in sentence case that says what it controls (“Keep the daemon running after Hutan quits”, “Cursor shape”), and one or two sentences of help in plain words: what it does and what happens when it is off. No key names, types or file paths of the source tree; a user-facing file such as `settings.toml`, `keymap.toml` or a command such as `hutan daemon stop` may be named.
- **A choice** shows a label per value (“Zed keys” / “Hutan keys”, “Keep in the menu bar”, “Last 24 hours”). The value written to the file is unchanged.
- Everything the sheet says besides fields (buttons, questions, notes of the pages above) is in `settings_labels::words` and read by the same tests; the sheet itself says where a value comes from instead of a generic “can be set per workspace” line.

## Time

One convention for every time a person reads, in the desktop and in the CLI (`hutan_client::time`):

- **Relative first.** Lists and headlines say how long ago: `3 min ago`, `2 h ago`, `yesterday`, `5 days ago`, `just now`; dense rows (the sidebar, overview cards, history hits) use the compact form `now`, `3m`, `2h`, `4d`.
- **Restart chips in a large sidebar (UX-C).** From 30 sessions on, a row's *respawned / resumed / interrupted / not restarted* chip is a small glyph (↻, ⚠ for the ones that need a look) whose tooltip has the word and the sentence, and the restart summary is one row (*39 sessions recovered — …* with *Review* and *Dismiss all*); *Review* opens the full list.
- **Absolute on demand, always local with its zone.** A tooltip on every relative time in the desktop shows `2026-10-17 14:31:07 +0500`: the date, the time to the second, the offset from UTC. Where there is no hover (the CLI, a one-line summary in a notification) a time that is stored is written absolute with the offset — `hutan search` and `hutan index status` (`2026-10-17 14:31 +0500`), `hutan recovery` (`hutand restarted at 2026-10-17 14:31 +0500, 3 min ago`), `hutan task show` (`ended 3m 5s ago (2026-10-17 14:31 +0500)`) — and the inspector's *Created* row gives both.
- Never UTC without saying so, never a clock time that leaves out the day. Durations (`ran 1m 5s`, an agent's elapsed time) are not times and stay as they are. The daemon's own timestamps (`created_at_ms`, `--json`) are Unix milliseconds; formatting is the client's job.
- Where it applies: the recovery summary (headline relative, tooltip absolute), the history panel and `hutan search`, `hutan task` (the Tasks panel shows durations, not times), the sidebar and overview rows, the inspector, the review batches of the Changes tray.

## Projects and workspaces (M12-B, rewritten by M18-B)

The daemon owns three kinds of container (ADR 0014, `docs/orchestration.md` § Projects and workspaces, protocol 0.19): **projects** (a repository with all its worktrees, or a folder — made by the daemon from the folders sessions start in), the user's own **workspaces** (a name, a pin, an order; they contain projects), and **Home**. The desktop decides how to show them. The sidebar is the `sessions` panel, titled **Projects**.

- **Organize** (`sidebar.organize`, header menu `⇅`, ⌘K "Sidebar: Organize by …"): `workspaces` (default) lists the user's workspaces (`▾ ★ NAME ·· ● 2  3`: chevron folds, `★` a pin, `● n` sessions that need you, the session count — `live/total` when they differ) with their projects indented below (`◧` repository / `▭` folder, name, the same counts) and the sessions under each project; then a **No workspace** label with the projects that belong to none, then Home (`⌂`) with its sessions. `projects` is a flat list of projects (pinned first) with their sessions, Home last; a session started straight into a workspace sits under that workspace's own row there. "Needs you" stays on top in both. A search or filter that names a workspace lists everything in it.
- **Session rows** take two lines: state dot, kind icon (`❯` terminal, `◆` agent, `✦` direct), title, agent badge, age; under them where the session is **now** (`SessionInfo.folder` — the shell's OSC 7 directory — else its start folder; the last two path parts, `muted`, the full path in the tooltip) and the **placement badge** (`main` or `wt <branch>`, a warning tint and `⚠` while another live agent works in the same checkout; the checkout's details in the tooltip). `panels.sessions.row` picks and orders the fields.
- **Sort** (header button `⇅ Manual ▾`, ⌘K, `workspaces.sort`) applies within each level — workspaces among themselves, projects inside a workspace, the projects in no workspace: `manual` (drag order) · `recent` (last activity) · `attention` (most sessions that need you, then activity) · `name`. Pinned rows come first in every mode and level; ties keep the daemon's order. Sessions keep creation order.
- **Drag** a row onto a row of the same level to reorder (manual only): it takes that row's slot within its group (pinned or not); the desktop sends `workspace.move`. **Drag a project onto a workspace** (or onto a project of another workspace) to put it there (`workspace.assign`); **drag it onto the "No workspace" label** — or onto a project in no workspace — to take it out. Home does not move and takes no drops. When the dragged project is part of a multi-selection, the whole selection moves.
- **Filters** (`workspaces.*`, the same header menu, ⌘K): hide empty / sleeping / CLI-created **projects** (workspaces and their members always show, as in `hutan ws ls`). Home, pinned projects and (`always_show_active`) the one you are in are never hidden, and a session that needs you stays visible in "Needs you" whatever the filters say. The filter box matches workspace and project names and folders, session titles, commands, folders and badges (any word order); a workspace or project that matches lists all its sessions. Hidden projects stay reachable through ⌘J.
- **Managing** (every change is a protocol 0.19 request): the header's **＋** menu — *New Workspace…* (a name; **Create**, or **Choose Folders…** to start with the projects of some folders: `workspace.create {name, paths?}`), *Open Folder…* (system panel → `workspace.open`, the project is shown), *Open Folder into Workspace ▸* (the same, then `workspace.assign`). **⌘-click** toggles a project in the selection and **⇧-click** selects the range from the last one clicked (neither opens it); right-click on a selected project → *New Workspace from Selection…* (`workspace.create {project_ids}`), *Move Selection to Workspace ▸*, *Remove Selection from Workspace*. **Workspace menu**: Pin/Unpin, Rename (inline: the name becomes a field, Enter or leaving it confirms, Escape cancels), Add Folder… (`workspace.add_root`), *Delete Workspace…* (asks, says that its projects and their sessions are kept and move to "No workspace"; `workspace.delete`), Reveal in Finder. **Project menu**: Rename (inline), Pin/Unpin, *Move to Workspace ▸* (the workspaces, or a new one), *Remove from Workspace*, Reveal in Finder, Close (greyed with the reason while sessions are live; asks first; the project stays in ⌘O). **Home menu**: Pin, Rename, Reveal; Home has one folder, set in `daemon.toml`.
- **⌘O** lists "Open Folder…" and the recent projects (`workspace.recent`, ▣ open · ▢ closed) with a fuzzy filter; Enter runs `workspace.open` and shows the project (inside its workspace when it has one). **⌘P** (Hutan base: ⌘J) lists every workspace, project (hidden ones included) and session in the sidebar's order — a project shows `workspace › folder`, a session `workspace › project · folder · badge · command`; Enter shows the container or the session's tab.
- **One layout per container** (`desktop-state.json`: `active_workspace` + `parked`): the center pane tree, docks, zoom, focus and preset belong to the container you switch to — the workspace when the project is inside one, else the project, else Home — and every project of a workspace shares it. Switching parks the layout and brings the other one back exactly. A container seen for the first time starts with a single pane under your dock arrangement. Opening a session of another container (sidebar, ⌘J, attention, a restored tab) switches first. A project that joins a workspace hands its layout over (the workspace's own wins when it has one); one that leaves keeps the workspace's. Terminals of parked layouts are **detached**, so nothing outside the window renders or is acked; coming back re-attaches with a daemon snapshot (two terminals ≈ 70 ms in the GPUI test, polling included), which is why attached-but-hidden was not chosen. A layout saved before workspaces existed belongs to the container of its first session (else Home); a container that vanishes hands the window to Home and its layout is dropped.
- **Settings**: user < workspace < project. The workspace layer is `<first project root>/.hutan/settings.toml` (Home: its folder), the project layer `<project root>/.hutan/settings.toml` of the **active project** — the project of the focused session, else the one last chosen in the sidebar (a workspace keeps the project chosen in it) — and a project in no workspace layers only its own. Both are hot-reloaded while they stay active, whether or not a session has focus (`crates/settings`: `set_workspace_root`, `set_project_root`, `get_for_roots`). Only workspace-overridable namespaces are read from either file (`terminal` today); the settings UI reports the rest.
- **New sessions** (⌘T, splits of a pane without a session) start in the active project, else the first project of the active workspace, else in Home; they get the `terminal.shell` / `terminal.env` of that project's layers.
- **Status bar**: the `workspace` item shows the active container; click opens the jump palette.

## Agent Session History panel (M17-B, rebuilt by OL-3)

The `history` panel is Orca's *Agent Session History* over the opt-in session history index (`docs/history-index.md`): a browsable catalog of the sessions on this machine — Hutan's own terminals and chats and the transcripts of Claude Code, Codex and OMP — from `history.sessions` (protocol 0.29), with full-text search (`history.search`) when a query is typed. It is the first (default) tab of the right sidebar's strip in every built-in preset, designed for its 350 px default width, and moves like any panel; ⌘K → "Panel: Open History" reveals it. The header says **Agent Session History** over *Indexed history* (below 300 px wide: **History**) and has the buttons *group by*, *sources*, *refresh* and ⚙ (the `history` settings).

- **Off** (`history.status.enabled = false`, the default): one card, **Show your agent sessions**: what Hutan would read, a checkbox per source (Hutan, Claude Code, Codex, OMP — the `history.hutan|claude|codex|omp` options of `daemon.toml`, as the daemon has them), one line on privacy (nothing leaves the machine; the index is one local file with secrets masked as far as possible; Clear deletes it), **Enable** and a link to the `history` settings. Enable writes the sources whose box differs from `daemon.toml` (`daemon.settings.set`), then `history.set_enabled`; it is off while no source is ticked. Nothing is requested, read or listed while indexing is off.
- **Scope**: a segmented **Workspace / Project / All** control (`HistoryScope`), its chosen segment on the neutral `right_sidebar_accent` surface like Orca's (orange is for the focus ring of the search field only). Workspace is the container the window shows (a workspace with its projects, a project, or Home); Project is the project the user is working in and is disabled while none is known; All has no restriction. Switching the shown workspace or project asks again by itself. The `history.project` setting (`workspace` | `all`) is where the control starts.
- **Search field** (32 px): typing narrows the catalog at once (`history.sessions` `filter`: case-insensitive words, all required, over title, first prompt, latest turns, agent, model, branch, directory and session id; `repo:` the project's name or folder; `path:` the directory or transcript path). A query that is not a `repo:` / `path:` filter is also searched in the transcripts after 250 ms of quiet, or at once on Return (`history.search`, 50 sessions): the ranked sessions replace the list, ungrouped, each with its best matching text and the matching words **emphasised** (the index's `«…»` markers are parsed, never drawn); the list bar says `N results`. Escape clears the field. The `history` settings `sources`, `roles` and `range` still narrow the transcript search (the sources menu changes the sources of both lists).
- **Scan strip**: `✓ Scanned 986 files · Just now` (the daemon's last complete scan; `Scanning 12 / 986…` while it runs, the panel asking again every 0.7 s). Click it for the per-source files, rows and last-indexed time, the index size and unreadable lines, and the index controls: **Clear…** and **Rebuild…** (both ask first; Clear deletes `index.sqlite`, Hutan's own records cannot come back) and **Disable** (`history.set_enabled` off: nothing new is read, the index stays; the off card returns).
- **List bar**: `2 of 250 sessions` (matches of the scope and filter, out of the sessions the catalog holds; when more match than rows were asked for: `250 of 1203 shown · 4000 sessions` and a **Show more** row asks for 250 more, up to 1000) and the sort dropdown, **Last updated** or **Created**.
- **List**: virtualized, grouped by project (the Hutan project or workspace the daemon attributed the session's directory to, else its folder), folder (last two path segments) or agent, in the order the daemon sorted the rows; a group header has its count and folds. A row: title (the source's own title, else the first prompt's first line; one line, "…" when it does not fit), the latest turn with its role — *You:* / *Agent:* / *Tool:* (a Hutan terminal: *Command:* / *Output:*) — clamped to two lines with "…" at the cut, and a meta line `glyph agent · N msgs · 3m ago · model` (the age's tooltip is the absolute time); at least 98 px high with 12 px of padding on both sides (text never touches the panel's edge), a hover fill, `⋯` and a chevron. A row that is open or selected has the subtle `right_sidebar_accent` fill, never the orange tint. During a full-text search the matching text replaces the latest turn and the meta says `N hits` and the folder.
- **Row details** (click the row; click again to fold): **Resume**, then **Copy Resume Command**, **Open Log**, **Reveal Log**, **Open Working Directory**, **Copy Session ID**; a card with the **first prompt**; the **latest** turns (up to three); the folder and branch. `⋯` and right-click open the same actions as a menu (plus **Copy Log Path**). There is no delete.
- **Actions**: **Resume** opens a new terminal tab running the session's `resume_argv` (OMP resumes by its absolute transcript path), greyed with the reason when the index has none (ids the resume rules refuse). It starts in the active workspace when one of its folders holds the session's directory, otherwise `workspace.open` finds or creates the workspace that contains it and the window switches there (Home is a catch-all and only counts for its own folder). A directory that no longer exists falls back to the active workspace's first folder, and a notice says so. **Copy Resume Command** puts the shell-quoted line on the clipboard. **Open Log** opens the transcript with the system, **Reveal Log** shows it in Finder. **Open Working Directory** shows the workspace that holds the folder (the most specific one; `workspace.open` makes one when none does). A **Hutan** row has **Open Session** instead of Resume (and no log): it shows the terminal or chat, and for a full-text hit on a terminal block scrolls the terminal to the block the hit names (`terminal_id` + `block_id`, exact: no time matching) while the session still has that terminal and the block; otherwise a notice says it is gone. ↑ ↓ select a row (group headers are skipped), ↩ runs its main action (Resume or Open Session; while a typed query has no transcript results yet, ↩ runs the search).
- **Live**: while the panel is visible it asks for the catalog again every 30 s and after the daemon's index status changes (it reads `history.status` twice a second while the index fills after Enable, Rebuild or Clear, until the row count stays put for 2 s, 10 s while empty, 60 s at most); a panel nobody sees asks nothing. The refresh button sends `refresh: true` (the daemon reads the changed transcript files before it answers) and, during a transcript search, searches again. A new answer keeps the list where it was: the view stays on the same row or group header even when rows come in above it.
- **Older daemon** (no `history.sessions`, protocol before 0.29): the panel is the search alone — the field, the scope control and the ranked results — under a note to restart the daemon; no catalog is requested.
- **⌘K**: "History: Search History…" reveals the panel and focuses its field. **⌘J**: a query that starts with `?` searches the history inline (top 8 sessions, the `history` settings' filters); ↩ resumes the hit when it can be resumed, else shows it in the panel with the field filled in and the row opened.
- **Settings** (`history`): `sources` / `roles` (default filters, empty = all), `range` (`24h` | `7d` | `30d` | `all`), `project` (`workspace` | `all`, where the scope control starts), `show_snippets` (the matching text under a transcript result) are the desktop's defaults; where the transcripts are belongs to `daemon.toml`. The page also has the On/Off of indexing itself (`history.set_enabled`, recorded in `daemon.toml`) with the indexed sources and the size, the same switch as Enable and Disable in the panel.

## Agent monitor (M13-B)

The `agents` panel (default: fourth tab of the right sidebar's strip in `default`, `focus` (hidden) and `overview`; movable like every panel) draws the daemon's observation tree (`agent.tree`, `agent.tree_updated`, `docs/protocol.md` § Agent observation tree) of every session that has one. The daemon owns the tree; the panel only filters and draws.

- **Sections**: one per session with agents, headed `workspace › session title` (click focuses the session), workspaces in the sidebar's manual order, sessions in creation order. Under it the tree, indented: main agent first, siblings in the order they started (then by id, so rows never jump when a node changes).
- **Row**: state glyph, name (else agent type, else the agent program or node id), the agent type when it is not the name, model, duration, and a second line with usage and the current activity (one line, 80 characters). Glyphs are never color alone: `●` working (accent, pulses once a second; steady under Reduce Motion), `▲` blocked (warning), `○` idle, `?` unknown (muted; the main agent after a daemon restart until it reports), and for finished agents their outcome: `✓` completed (success), `✗` failed (error), `⊘` interrupted, `⏹` disconnected (muted). Finished rows are dimmed.
- **Duration** runs from `started_at_ms` (to `ended_at_ms` once finished). It refreshes once a second only while the panel is drawn and a listed agent works or waits for the user; a hidden panel, or a tree with nothing running, wakes nothing.
- **Usage** is labelled by `usage.kind`, never zero-filled: `1.5k tok · context` (`claude.context`: the latest context, `3.2k/200k` with a window), `3.2k tok · total · $0.31` (`codex.total`), `800 tok · excl. cache` (`omp.tokens` leaves cache reads out). A kind this build does not know shows as the source sent it.
- **Finished subtrees** (a finished agent whose descendants all finished) are hidden unless `agents.show_finished`, and then folded into their head row (`▸ +3`; the chevron unfolds). A finished agent with a live descendant stays, so the tree stays connected. **Blocked agents are always listed**, with the agents above them.
- **Chips** at the top toggle `agents.show_finished`, `agents.only_blocked` and `agents.scope` (this workspace · all workspaces) in `settings.toml`. ⌘K: *Agents: Show Monitor*, *Agents: Show Only Blocked*.
- **Click** a row to focus its session (its terminal or chat tab). **Right-click**: Open Session, *Open Transcript* (only for nodes that reported a `transcript_path`; reveals it in Finder), Copy Node ID.
- **Badges**: a session's row in the sidebar, its tab and its overview card show `↳2` (subagents working or waiting for you) and `▲` (any node blocked); the tooltip says `2 subagents running` / `Blocked: reviewer`. Nothing is drawn while there is nothing to say.
- **Attention**: a live session with a blocked node always needs the user (`SessionsStore::presentation` folds the tree over the session summary), so the sidebar's *Needs you*, ⌘⇧J, the workspace counts and the status bar's `attention` item include it. `SessionsStore` emits `AgentAttentionChanged { session_id, blocked_nodes }` when a node becomes blocked (not for what was already blocked when the window connected, not when one is resolved); M6 turns it into a macOS notification.

## Resources panel (M13-D)

The `resources` panel (default: the last tab of the right sidebar's strip in every preset, the bottom dock stays empty and hidden; ⌘K *Resources: Show Panel*, ⌥⌘R toggles it; movable like every panel) is the Activity Monitor of Hutan: what each live session runs and what it costs, over the resource manager of `docs/resources.md`. The daemon samples; the panel draws and sends the actions.

- **Subscription**: `resources.subscribe` (`resources.interval_ms`, default 2000, at least 1000) on the window's shared connection (`Connection::resources_subscribe`; it remembers the subscription and renews it after a reconnect, announcing samples as `Notice::ResourcesUpdated`). Every draw marks the panel as seen; a 500 ms watcher unsubscribes at the first look that finds it was not drawn (hidden dock, another tab, closed) and the next draw subscribes again, so a hidden panel costs the daemon nothing (it stops sampling with its last subscriber). The link retries with backoff when the daemon goes away (the panel says so) and says so when the daemon is older than protocol 0.18.
- **Table**: a `hutand` row first, then one block per session that has a process: fold chevron, title, folder, placement badge (`main` / `wt <branch>`), and its totals in the same columns (processes, CPU, memory). Under it the process tree, indented by depth: name, PID, CPU % (a thin `accent_soft` bar, 100 % = full, more is clipped), memory (MB, GB from 10 000 MB), threads, command (one line, the full text in the tooltip). A process that an agent reported with `--pid` carries an `accent` dot and the node's label; agent nodes without a process of their own (a Codex subagent thread, an agent that reported no pid) are listed under the tree. A stopped or zombie process says so in a pill. Narrower than 520 px the threads and command columns are left out.
- **Sort and filter**: the CPU, Memory and Name headers sort (`resources.sort`, default `cpu`: busiest first); a second click on the active header flips the order. Sessions are ordered by their totals (or title) and siblings inside a tree by the same key, so the tree stays a tree. The filter box matches names, command lines, pids, agent labels and the session's title, workspace and folder, case-insensitively: matches keep their ancestors, a session match keeps its whole tree; folds are ignored while filtering.
- **Actions** (⋯ on a row, or right-click; the menu starts with *Open Session*): per process *Interrupt (SIGINT)*, *Terminate (SIGTERM)*, *Kill (SIGKILL)…*, *Stop (SIGSTOP)* (*Continue (SIGCONT)* while it is stopped); per session *Stop Session…* and *Kill Session…*; per agent node *Interrupt Agent* (the main agent: a chat's turn, or ^C in a terminal; a subagent that is a process: SIGINT, with *Interrupt Agent, Then Terminate* for SIGTERM two seconds later), greyed out with the reason for a subagent the harness cannot stop on its own (no process, and not a Codex thread). `…` asks first: Kill names the process and the descendants that keep running, Kill Session lists what dies (up to 12 names), Stop Session says SIGHUP then SIGKILL after 3 s; the rest go at once. The daemon's refusals (`forbidden`, `invalid_state`, `not_found`) appear on the row (`forbidden: …`; click to dismiss). A few hundred milliseconds after an action the panel asks for a fresh sample instead of waiting for the next one.
- **Show in Resources**: an entry of the session context menu (sidebar, overview card, tab) reveals the panel, clears the filter, unfolds the session, highlights it (`accent_soft`) and scrolls to it; a session without a process says so.
- **Empty states** (the mascot): nothing running (sleep), reading (thinking), no match for the filter, daemon too old (error), daemon unreachable (sleep).
- **Settings** (`resources`): `interval_ms`, `sort`. Visual tokens only: `ui.rs` components, `accent`, `accent_soft`, `warning`, `danger`, `muted`.
## Tasks panel (M7-C)

The `tasks` panel (default: the Tasks page of the left sidebar in `default`, `focus` (hidden) and `overview`; movable like every panel) draws the daemon's tasks (`docs/orchestration.md` § Tasks) as a dependency tree. The daemon owns tasks; the panel sends `task.*` requests and draws what `task.updated` / `task.removed` say.

- **Tree**: every task once, under the dependency it waits for last (the others read `also after …`), dependencies before dependants, roots oldest first. Row: state glyph and color from the theme — `○` todo (muted), `◎` ready (accent), `▶` running (accent, pulses once a second; steady under Reduce Motion), `◆` blocked (warning), `✓` done (success), `✗` failed (error), `⊘` cancelled (muted) — the title, the state name, and a second line with the placement badge (`main` / `wt <branch>`), the run (`shell`, `$ command`, `codex chat`, `omp chat`), `auto`, the elapsed time (running time, or how long it took; ticks once a second only while the panel is drawn and a listed task runs) and the outcome (`(exited 3)`). Done and cancelled rows are dimmed.
- **Click** a row: select it and focus its last session's tab. The selected row shows its actions, exactly the ones the daemon accepts in that state: *Start* (ready), *Done* (everything but done and cancelled), *Cancel* (todo, ready, running, blocked), *Reset* (failed, done, cancelled), *Delete* (not while running); *Changes* opens the Changes view (`docs/orchestration.md` § Review and merge) of the checkout it ran in. Deleting a task others wait for asks first: the daemon's refusal names them, *Delete them too* repeats it with `cascade`.
- **+ New task** (header, or ⌘K *Tasks: New Task…*): opens the **Start…** sheet on *Queue as task* (below); the panel has no form of its own. Row actions, the *Changes* button and the dependency tree are unchanged.
- **Filters**: chips at the top toggle `tasks.show_done` (done and cancelled rows; failed ones always stay, and tasks that waited for a hidden one move up as roots) and `tasks.scope` (`workspace` — the window's — or `all`).
- **Empty state**: the mascot, a sentence and *+ New task* (the Start… sheet); "Nothing matches the filters." with the success mascot when only the filters hide tasks; a note when disconnected or when the daemon predates tasks.
- **Commands** (⌘K): *Tasks: New Task…*, *Tasks: Show Panel*, *Worktree: Review Changes…* (a picker of the checkouts of the workspace's repositories, main checkouts included).

### Changes view

**Narrow centers (UX-C).** Below about 720 px of center width the file list is a one-row strip above the diff (the selected file, `n of m`, counts; click it to unfold the list, choosing a file folds it again), the diff has the whole width and long lines scroll sideways inside it, and the scope card's *Commit all…* wraps under its sentence instead of leaving the pane.

A center tab per checkout (title `Changes · <branch>`): the header names the branch and what it is compared against (`against main (merge base 5b1c0f3)`), the file and line totals, *With uncommitted* (toggles `include_uncommitted`), *Refresh* and *Merge into main…*. Left, the file list: status letter (A/M/D/R, colored), path (`old → new` for a rename), `+n −m` or `binary`; right, the selected file's unified diff in the mono font, drawn with the chat's diff rows (`hutan_chat_view::diff` parsing, theme colors), one collapsible block per hunk (`▾`/`▸`, a folded hunk says how many lines it hides), 500 lines per file at most, a note when a patch was cut at 2 MiB. Read-only. *Merge into main…* (worktrees with a branch only) opens a card: strategy (Merge commit / Squash / Rebase), the commit message (not for rebase), Merge / Cancel. A refusal stays in the card — the daemon's reason, and for a conflict the files with `⚠` — and changes nothing; success shows a toast (`Merged feature/x into main (9d8c7b6)`) and a card offering *Remove worktree and branch* (`worktree.remove` with `delete_branch`, forced only when the worktree still has uncommitted changes that were not merged, as the button says) or *Keep*; removing closes the tab.

**Delivery (DL-1, protocol 0.25).** A task row carries a delivery pill after its title — `Awaiting review` (accent), `Accepted` / `Merged` (success), `Changes requested` (warning, with the note under it), `Discarded` (muted) — and a done task that waits for review is neither dimmed nor hidden by *Show done*. The session of a task awaiting review is a soft *Needs you* item (label `review`, accent, filled dot; no Dock or menu bar badge). The Changes view header shows the pill of its task, then *Accept*, *Request changes…* (a note form) and *Merge into …*; under it a card says what a merge takes (*Committed on <branch>*: commits and files) and what it does not (*Uncommitted in the worktree*, with *Commit all…*); after a merge that left uncommitted changes the cleanup card makes *Keep* the primary button. Behaviour and tests: `docs/orchestration.md` § Delivery.

## Terminal blocks (M10-C)

The daemon indexes OSC 133 marks into blocks (protocol 0.5); the desktop draws them **over the single grid** — the grid, its columns and the PTY size never change, and `terminal.blocks = false` makes the terminal look exactly as without blocks (hot-reloaded).

- **Gutter**: a thin bar at the left edge, inside the terminal padding (width = the `block gutter bar` density token), one per block from its prompt to the end of its output. `--success` after exit 0, `--error` after a failure, neutral (`--muted`) while the command runs and when no exit code is known; the bar of a running block pulses slowly (steady under Reduce Motion, read from macOS; `HUTAN_REDUCE_MOTION=0|1` overrides). The open prompt has no bar.
- **Separator** above every prompt but the first row, and a right-aligned **chip** on the prompt line: `✓ 1.2 s`, `✗ 2 3 m` (exit status, duration). A chip never covers text the prompt line shows: when the line is too full for it, it is left out.
- **Hover** a block: a faint tint and an actions bar at the right of its first visible row — Copy command, Copy output (both through `terminal.block_text`, so they work beyond the loaded scrollback), Collapse / Expand, Rerun (types the command and Enter into the session; off while a command runs), Send to agent (a chip in the composer of the last-focused chat, `docs/agents.md`).
- **Collapse** folds a finished block's output rows into one summary row (`… 241 lines, exit 0`) *in the view only*: nothing changes in the PTY, the scrollback or what other clients see, and older lines move down to fill the freed rows (the view stays anchored at its bottom). Folds apply to the pane, not to overview previews; they are saved per terminal in `desktop-state.json` (`collapsed_blocks`, terminal id → block ids) so they survive a desktop restart, and are forgotten with the terminal.
- **Keyboard**: ⌘↑ / ⌘↓ move the highlight to the previous / next prompt and scroll to it (past the newest block ⌘↓ returns to the live screen; typing clears the highlight); ⌘⇧A selects the current block's output, ⌘⇧C copies it. The *current* block is the highlighted one, else the newest command with output. Every action is also in ⌘K under `Block:`.
- **Overview previews** show the gutter colors only (no chips, separators or folds). The inspector's Info tab lists the last 20 commands of the focused terminal (command, exit, duration, cwd); a click scrolls that terminal to the block.

## Accounts and the assistant (M8-B)

- **Accounts** (Settings sheet → `accounts`, ⌘K "Accounts…"): one card per harness (OMP, Claude Code, Codex): name, a detection pill (installed / not found with the daemon's install hint), **Log in** and **Log out** buttons, then the accounts it reports (label, provider, plan pill, status pill; OMP has a Log out per provider). The login runs in a terminal session which the window shows (the sheet closes); `instructions` (OMP logs out inside its interface) appear as a notice. No token or address is ever shown. Empty state: the mascot when no harness is installed.
- **Start…** (S-1; ⌘⇧T in both keymap bases, ⌘K *Start Agent or Task…*, the ▶ of a project row and the sidebar header, the Tasks panel's *+ New task*, the empty pane/project): one sheet for every start. Fields in Tab order: *Project* (fuzzy picker, shows its folder and branch), *What* (Agent chat · Terminal agent · Plain terminal · Command), *Agent* (OMP / Claude Code / Codex / direct providers; the choices keep their order and place whatever the harnesses report; one that is not installed or logged in is dimmed and cannot be started, and clicking it says why on a line *below* the row with *Install…* / *Log in…* for that agent only), *Model* (the harness's list; default the harness's own), *Prompt* (multi-line, Vim mode when on), *Where* (Main checkout · New worktree with branch `hutan/<slug of the title>` from the current branch · an existing worktree; a warning when another live agent already works in the chosen checkout), *Title* (from the prompt), *More* (After; the system prompt of a direct chat), *Start now* / *Queue as task*. ⌘↩ starts, ⌥⌘↩ (*Start & new*) starts and keeps the sheet with project, what and where for fan-out. Every start is `task.create` (+ `task.start` for Start now) so it shows in the Tasks panel and the monitor; only a direct model chat, which tasks cannot run, uses `chat.create`. A model picked for a harness chat is applied with `chat.set_model` after the first message (tasks carry no model); a queued task starts on the harness's default. *Switch model…* of a running chat is its own small dialog.
  **Safe Start (UX-B).** The sheet opens on what was started last (What, Agent, Model, Where; kept in `desktop-state.json`), and on **Plain terminal** before there was a first start; a preset about an agent (a prompt, *Queue*, a direct chat) opens on *Agent chat*. The footer says what Start will do in one line (*Start OMP · Claude Opus 4.5 chat in hutan (main)*, *Open a terminal in hutan (main)*, *Run “cargo test” in hutan (main)*) and the Start button names the agent. An agent chat that sends its prompt at once asks first, once per agent and model: *This sends your prompt to OMP · Claude Opus 4.5 now* with *Start* (↩), *Cancel* (Esc) and *Don't ask again for this model* (unticked until the user ticks it; Space toggles); `launcher.confirm_agent_start = false` turns the question off. Plain terminals, commands and queued chats never ask. Keyboard: Tab / Shift-Tab move between rows and the focused row is tinted with an accent border; on the choice rows (What, Agent, Model, Where, Start now / Queue) ↩ or Space opens the choices, ← → ↑ ↓ change the value only while they are open, Esc or Tab closes them, and ⌘↩ does not start while they are open. Only the chosen agent says why it cannot start; choosing another agent clears the previous one's error. **Command** is shell text: the sheet runs `$SHELL -lc "<text>"` (`/bin/zsh` without `$SHELL`) in a session with the login environment, so `;`, `&&`, pipes, quotes and globs work (the field says *Runs in your shell*); `hutan task add … -- <argv>` stays an explicit argv. The Changes view header wraps its actions under the title when the pane is narrow, so *Merge into …* is never cut off. Rename (session, project, workspace) opens with the whole old name selected: typing or pasting replaces it.
- **Mawas Home**: the scope dropdown, a backend row (`harness · model ▾`) opening the backend and model chooser, and — when nothing is configured and no harness or provider is ready — the first-run card (mascot, *Use OMP* when OMP is installed and logged in, *Add a provider…*); otherwise the daemon's automatic backend and model open it (`docs/assistant.md` § Backends). `assistant.enabled = false` (the default for a new settings file) removes the page, its nav row, its commands and the system-wide shortcut; on, Mawas Home is a center tab opened from the *Mawas* nav row (§ Layout).
- **Memory** (MW-2; a center tab like Mawas Home, `docs/assistant.md` § Memory): a header row with the title, the scope filter (*All* / *Global* / the shown workspace's name), *Show archived* and **Add**; below it a column about 46 rem wide: *Waiting for you* — one accent-bordered card per memory proposal (what Mawas proposes and where, the text, "Now: …" for a change, **Approve** / **Edit & approve** / **Reject**) —, the editor card while one is open (scope and category chips for a new entry, a multi-line field, "At most 2 KiB. Memory is not a secret store…", **Save** / **Cancel**, ⌘↵ saves, Esc cancels), then the entries grouped under *Profile*, *Facts*, *Notes*: the text, a pill with where it applies, the revision (`r2`), an **inferred** pill (warning tone) while a model wrote it and the user has not confirmed it, the provenance in plain words, and the actions *Confirm* (inferred), *Edit*, *Archive* / *Restore*, *History* (the revisions open under the row). An empty memory says what it is for; a failure shows in the daemon's words in the `danger` tone above the list. No timer: the page lists when it is shown and when the daemon sends `memory.changed`.

## First-run setup (D-2)

A centered sheet in the v2 style (surface card, 12 px radius, serif headline, sans body, a mascot image per step, step dots, Back / Continue / Skip setup) shown once, until `app.setup_done`; reopened from ⌘K **Hutan: Run Setup…** and Settings → app → **Run setup again**. Code: `workbench/setup.rs`; the controls are `ui.rs` components on theme tokens.

1. **Welcome** — mascot (`angle-front`), "Code from the canopy".
2. **Look** — appearance System / Light / Dark (a mini preview of each) and density Compact / Comfortable; the window previews the pending choice live and returns to the settings when the setup is skipped.
3. **Agents** — OMP, Claude Code, Codex with detection and logins from `account.list`; **Log in…** starts the harness's own login (the setup pauses behind the terminal; ⌘K **Hutan: Run Setup…** continues it); a missing harness offers **Install…** (below) and an install guide link. Hooks are opt-in (UX-B): one unticked **Install hooks for OMP / Claude Code / Codex** checkbox per installed agent, each saying what it writes and where (`~/.omp/agent/extensions/hutan-agent-state.ts`, `~/.claude/hooks/…` + `settings.json`, `~/.codex/…` + `hooks.json`; with a custom `HUTAN_HOME` the step says the hooks still go into the agents' real configuration folders); leaving the step installs the ticked ones and nothing else. *Keep hooks installed on every start* (`integrations.auto_install`, default **off**; Codex asks once to trust new hooks) and per-harness **Uninstall hooks**. The assistant: enable, and the backend (default OMP when it is logged in, else none; `assistant.configure`, global scope).
4. **Background** — *Keep the daemon running after Hutan quits* and *Start Hutan's daemon when you log in* (both off; "Like Orca and Paseo, the daemon runs while the app runs"), **Allow notifications** (the permission is asked for only when the button is pressed, in the app's identity which the daemon shares, and the answer is shown) and the optional global Mawas shortcut (off, ⌥ Space or ⌘⇧ Space).
5. **Command line** — *Add `hutan` to your PATH*: a symlink `~/.local/bin/hutan` → `Contents/MacOS/hutan` of the running bundle, off by default, made only when the folder exists and the name is free or already this link (`cli_link.rs`); otherwise the command is shown with Copy. A note says shell integration needs nothing in the rc files.
6. **Done** — mascot (`state-success`), *Open a folder* (⌘O) and *New terminal*.

Every choice is written when its step is left (Continue or Back); Skip writes `app.setup_done` and leaves what was not written at its default. Not part of any test: the real `SMAppService`, notification permission, `~/.local/bin`, `~/.omp` / `~/.claude` / `~/.codex` and npm — each is behind a fake (`crate::macos`, `cli_link`, `integrations`, `harness_install`).

The Agents step also carries one line, **Send messages with** (⌘Return / Return, `chat.send_key`, § Send key): it is saved when pressed, and leaving the step counts as having been asked (`chat.ask_send_key` goes off). A setup that was skipped, or finished before the question existed, gets the same question once, in one line above the first composer that gets the keyboard (no modal; ⌘Return / Return / ×).

**Agent integrations never install by themselves** (`integrations.auto_install`, default **off**; a setup that never chose is off too): only after the user turned it on, once the daemon answers, off the main thread, the desktop runs `hutan integration install <harness>` (the CLI beside the app) for each harness the Accounts page detects whose `status` is not `installed`. Results go to the log; failures show one notice with the commands. Off stops future installs; it never uninstalls.

**Install… for a missing harness** (setup, Accounts page, new-agent dialog, the assistant's failure card for "was not found"): the package is checked against a fixed table (`harness_install.rs`: Claude Code → `@agentclientprotocol/claude-agent-acp`, OMP → `@oh-my-pi/pi-coding-agent` with `bun`, Codex → `@openai/codex`) with `npm view <pkg>@latest name version repository.url bin dist.integrity --json`; the name, repository and executable must match, else nothing is offered. A confirmation shows the version, repository and integrity; on confirm a terminal tab runs `npm install -g <pkg>@<version>` (`bun install -g` for OMP) and when it exits 0 the detection asks again (the failed assistant offers Retry). Without npm the command and the source link are shown. Nothing is downloaded without a click and `npx` is never used.

## Daemon lifecycle

**The daemon follows the app**, unless the user asks it to outlive it. The app starts `hutand` when none is running; it keeps it for as long as Hutan runs (a window is open, or Hutan sits in the menu bar); quitting stops it. Two separate, opt-in choices change that (both default off, like Orca and Paseo where the daemon runs while the app runs and nothing starts at login): `app.keep_running` ("Keep the daemon running after Hutan quits": quitting leaves `hutand`, no login item) and `app.start_at_login` ("Start Hutan's daemon when you log in": the bundled launch agent, `SMAppService`). A `settings.toml` from before D-2 with `keep_running = true` (it meant the login item) is migrated once to both keys and a settings diagnostic says so.

**A daemon that outlives the app is never invisible** (M6-D): inside `Hutan.app`, `hutand` puts its own icon on the menu bar whenever no desktop is connected and takes it off the moment one connects, so the menu bar shows exactly one Hutan icon, the desktop's while the app runs and the daemon's while only the daemon does (§ macOS integration → *The daemon's menu bar item*). It is not optional and is not affected by `app.menu_bar`.

```mermaid
flowchart TD
  L[Launch Hutan] --> I{another Hutan in this data folder?}
  I -- yes --> F[Bring it forward, exit 0]
  I -- no --> C{hutand running?}
  C -- yes --> A[Connect]
  C -- no --> S[Start hutand next to the app binary] --> A
  W[Close the last window] --> M{app.on_last_window_close}
  M -- quit --> Q
  M -- "menu_bar (needs app.menu_bar)" --> H[Hide: no Dock icon, menu bar item stays, daemon keeps running]
  H -- "Open Hutan, shortcut, second launch" --> A
  Q[⌘Q, menu bar Quit, Dock Quit] --> KR{keep_running, or start_at_login active?}
  KR -- yes --> K[Quit; hutand keeps running]
  KR -- no --> LV{live sessions?}
  LV -- no --> X[Stop hutand silently, quit]
  LV -- yes --> D["Quit Hutan and stop N sessions?"]
  D -- Stop Sessions & Quit --> X2[Stop sessions, stop hutand, quit]
  D -- Keep Running in Background --> R[Turn keep_running on; no login item] --> K
  D -- Cancel --> A
```

- **Closing the last window** (`app.on_last_window_close`, default `quit`): `quit` is ⌘Q (the window stays until the flow has decided). `menu_bar` keeps Hutan alive in the menu bar: the window is hidden, not destroyed (it holds the connection, the sessions and the layout the menu bar item serves), the activation policy becomes *accessory* (no Dock icon, no application menu), and the daemon keeps running until Quit. Opening Hutan from the menu bar item, the global shortcut, a notification, a second launch or the Dock brings back the window and the Dock icon. `menu_bar` needs `app.menu_bar = true` (nothing would be left to reopen Hutan from): without it closing the last window quits, and the settings show a diagnostic for `app.on_last_window_close`. Turning the menu bar item off while Hutan is hidden shows the window again. Closing any other window just closes it; closing a tab or pane only detaches.
- **Quit** is the same flow from ⌘Q, the app menu, the menu bar item and (best effort) the Dock. From the menu bar item or the Dock the window comes forward with the question (it may be hidden, minimised or behind other apps), so the click never looks like it did nothing; a second Quit while the first is still ending the process asks the platform again. With *keep running* off the daemon stops with Hutan: silently when no session is live, after the confirmation when some are (the title counts them: "Quit Hutan and stop 1 live session?"). *Keep Running in Background* turns on `app.keep_running` only (nothing is registered with the system, so it works outside Hutan.app too) and quits, leaving the daemon; it stops when you stop it (Daemon menu, `hutan daemon stop`) or at logout. With `keep_running` on, or with `start_at_login` *active* (registered and approved), nothing is asked and nothing is stopped, even with no live session.
- **The Dock's Quit, logout and shutdown** reach the app as a platform termination with no window to ask in, so the app cannot ask: an idle daemon is stopped as with ⌘Q, one with live sessions is left running with them (as after a signal). [The Dock's Quit could be routed through the confirmation by taking over `applicationShouldTerminate:` from GPUI's delegate; that is not done because the same hook decides whether logout may proceed.]
- **SIGTERM / SIGINT / SIGHUP** quit cleanly without touching the daemon, never asking (unchanged): layout saved, connection closed, exit code 0.
- **Single instance.** One Hutan desktop per data folder (`HUTAN_HOME`, else `~/Library/Application Support/Hutan`). `LSMultipleInstancesProhibited` makes Launch Services reactivate the running app for the Dock and Finder; the executable enforces it itself too (running the binary directly, `open -n`): it takes an exclusive `flock` on `desktop.lock` in the data folder before any window opens, and a second desktop that finds it held connects to `desktop.sock` (owned by the first, `0600`, peers of another user dropped), sends `{"cmd":"activate"}` and exits 0 at once. The first one brings its window forward (from the menu bar, from behind other apps), or opens a new one if there is none. A lock or socket left by a crashed desktop is harmless: the lock goes with the process and the lock holder replaces a stale socket file. Code: `crates/desktop/src/instance.rs`.
- **Daemon controls** — the same set in three places: the `Daemon` menu (menu bar), a popover opened by clicking the `connection` status item, and ⌘K commands:
  - status: connected/reconnecting, daemon version + protocol, pid, uptime, sessions (live / total), clients, socket path, log path;
  - actions: Restart Daemon, Stop Daemon (both confirm when sessions are live: they stop; the app stays open and shows "daemon stopped" with a Start button), Start Daemon (when stopped), Open Logs;
  - the two switches **Keep daemon running after Hutan quits** (`app.keep_running`) and **Start daemon at login** (`app.start_at_login`, with the system's status when it is not plain `enabled`), and *Open Login Items* (enabled while the status is `requires approval`).
- The CLI (`hutan daemon start|status|stop|restart`) controls the same daemon; the app reflects CLI-initiated stops/restarts through its reconnect state. The *stopped* state that **Stop Daemon** sets (it keeps the connection from starting a daemon) ends whenever a daemon of another epoch connects, whoever started it (the CLI, autostart, another client), and also when **Start Daemon** finds the daemon running: terminals accept input again without a restart.
- Quitting is always clean: layout saved, connection closed, exit code 0; the daemon is stopped through `daemon.shutdown`, never killed. A desktop panic writes message + backtrace to `desktop.log` before exiting.
- A `settings.toml` from before this model may still have `app.on_quit`: the key is gone, so it is ignored with the settings' "unknown key (ignored)" warning, not an error.

## macOS integration (M6-C)

Code: `crates/desktop/src/macos/` (the four system APIs, each behind a trait with a fake) and `workbench/mac.rs` (the workbench side). `MacServices::system()` builds the real ones and only `app::run` calls it; every GPUI test uses the fakes, so no test touches `SMAppService`, `NSStatusBar`, the Dock tile or the hotkey API.

- **Start daemon at login** (`app.start_at_login`, Settings → `app`, the `Daemon` menu, the daemon popover and the menu bar menu; it was `app.keep_running` before D-2). On → `SMAppService.agent(plistName: "dev.hutan.daemon.plist").register()`; off → `unregister()`; the file is the truth, so editing `settings.toml` does the same and a window opened with the setting on registers the agent. The status sits next to the toggle: `enabled`, `requires approval` (button *Open Login Items* → `SMAppService.openSystemSettingsLoginItems()`), `not registered`, `not found` (a registration that ends `not found`, or that the system refuses, is reported and the setting goes back to off). It is re-read when the window becomes active, so approving in System Settings is picked up. Only available when the executable is `*.app/Contents/MacOS/*` **and** the main bundle is `dev.hutan.Hutan`; elsewhere the toggle is disabled with "available in Hutan.app" and nothing is registered.
  - *Active* means registered **and** approved (`enabled`). While active: quitting never asks and never stops the daemon (a daemon without live sessions would otherwise stop with the app), and at start the desktop does **not** spawn `hutand`: its connection is `Connection::start_held`, which only dials. If no daemon answered after 4 s (the user stopped it, or the login item has not run yet) the connection may start one as before (`launchd_grace_over`), unless the user stopped the daemon from the app.
  - Turning it off while sessions run: the app unregisters the agent and the daemon is whatever launchd leaves running; from the next quit on, the daemon follows the app again (§ Daemon lifecycle). [INFERENCE: whether `unregister()` also ends the running agent is not documented by Apple; the integrator's demo checks it, `docs/packaging.md` § Keep running.]
- **Menu bar item** (`app.menu_bar`, default on): the orangutan glyph (`assets/macos/MenuBarIcon.png` 18 px and `@2x` 36 px, black template image tinted by the system, rendered from `assets/brand/05-mascot/svg/favicon.svg` by `just menubar-icon`) and, when something needs you, its count. Menu: the daemon state (`hutand connected` / `stopped` / `reconnecting…`, or the operation in progress), `N need you` with one item per session (title — blocked / approval / failed: detail; at most 10, then "…and N more"; a click brings the window forward and shows the session, a Mawas conversation on Mawas Home), *Open Hutan*, *New Terminal*, *Keep Daemon Running After Hutan Quits* (checkbox, same rules as above), *Quit Hutan* (the usual quit flow, with the window shown first when it has to ask). The item is created when the setting is on and removed when it goes off; hot reload, no restart. `StatusItemModel` is pure data (count, menu rows) derived from the attention model.
- **The daemon's menu bar item** (M6-D; `crates/daemon/src/menu_bar.rs`): so that a daemon that outlives the app (*keep running*, *start at login*) is never invisible, `hutand` shows an item of its own whenever **no desktop is connected** (a `hello` with `client.kind = desktop` counts from the handshake until the connection closes), and only when it runs from `Hutan.app` (a `cargo run` daemon or a test shows none and touches no Cocoa). **One Hutan icon at a time**: "a desktop" is a desktop *process*, not only a connection: before it shows its item the daemon also looks at the desktop's single-instance socket (`desktop.sock` in the data folder) and, while a desktop listens there (reconnecting after a daemon restart or a dropped connection), looks again every second instead of showing a second icon. A connected desktop shows its own item (above; richer: the toggles, *New Terminal*, the unread failures it knows) and the daemon's goes away at once; when the last desktop goes, the daemon's item appears after 750 ms, so that a desktop that is starting (it started the daemon) or reconnecting after a dropped connection never meets a second icon. `app.menu_bar = false` hides the desktop's item only; the daemon's is **not optional** (you asked to always see that the daemon is on) and has no setting. Same template glyph; a small dot (`•`) next to it while something needs you. Menu: `Hutan daemon — running · N sessions`; `N need you` with one item per session (title — approval / blocked / failed: detail, at most 10, as above); *Open Hutan*; *Stop daemon…* (straight away with no live session, otherwise the alert *Stop the Hutan daemon?* with *Stop Sessions* and *Cancel*); *Start at login* (a checkmark that shows `app.start_at_login` read from `settings.toml`; read-only, a click opens Hutan's settings). What needs you is what the daemon knows, one entry per session, strongest first: a pending permission request (`approval.list`), a blocked agent (the session's agent report), and a session that exited with an error since the last desktop went away (a pending `session_exited_with_error` intent: the desktop knows what you have seen, the daemon only what happened while you were away, so this one follows `[notify]`). *Open Hutan* and the entries launch the app (`open -n Hutan.app`, with the daemon's `HUTAN_HOME` / `HUTAN_SOCKET` passed on; the desktop's own `desktop.lock` makes a second desktop bring the first one forward and exit) and, once a desktop window has published (`ui.publish`: it is synchronised), ask it for the session (`ui.open`; the intent is acknowledged) or the settings (`ui.run` of `workbench::OpenSettings`), at most 30 s after the click. Stopping from this item is the same orderly stop as `hutan daemon stop --stop-sessions`. Details and the main-thread loop: `docs/packaging.md` § The daemon's menu bar item.
- **Dock badge** (`app.dock_badge`, default on): `NSApp.dockTile.badgeLabel` is the number of sessions that need you, empty at zero. The same number as the menu bar item and the same rule: **blocked** (an agent in the session is blocked, or the session waits for an approval), **pending approvals** (a session with a permission request counts once, however many requests), and **unread failures** (exit ≠ 0, a signal or a spawn failure that the user has not looked at). A finished-and-unread session (`done`) is in the sidebar's *Needs you* but not in the badge: a badge says "go and decide" or "something broke". Opening the session marks a failure read; the count drops with it (`SessionsStore::attention_entries`, `attention::classify`).
- **Global shortcut** (`assistant.global_shortcut`, empty by default = off; e.g. `alt-space`): a Carbon hot key (`RegisterEventHotKey`), so no Accessibility permission is needed and it works while any app is in front. Pressing it brings the app forward and runs `OpenAssistant` (the same code as ⌘K → Mawas: Open). Syntax: GPUI keystrokes (`alt-space`, `cmd-shift-k`, `ctrl-f12`); at least one of `cmd`, `ctrl`, `alt` is required so a bare key can never swallow typing everywhere. When registration fails (another app owns it) the window says so **once** and the setting is left as is; changing the setting tries again, other settings reloading does not. An invalid value gets its own notice. The in-app keybinding (`keymap.toml`) is independent of this setting.
- **Events** (a menu choice, the shortcut) travel from the main-thread callbacks to the workbench over one bounded channel (`MacEvent`), drained in a GPUI task; a full queue drops the newest. Nothing is polled.

## Text input (TI-1)

Every Hutan text field (composer, palette and pickers, prompt dialogs, settings sheet, Start sheet, sidebar filter and rename, History and Resources filters, merge/commit/comment forms, question cards, attach form) is one element, `workbench/text_field.rs` (`EditableText`), over `TextInput` or a `FieldEdit` that mirrors a `String`. While its owner's focus handle has the keyboard it registers the platform text input handler: **text only arrives through `replace_text_in_range`** (plain keys, input methods, dictation, press-and-hold accents, the emoji picker, synthesized text such as accessibility tools), in UTF-16 ranges. `replace_and_mark_text_in_range` underlines the composing text and the commit replaces it in one undo step; `bounds_for_range` returns the caret rectangle, so candidate and accent popups appear at the caret. `on_key_down` keeps the editing keys (`edit_keys`): arrows, ⌫, Return in multi-line fields, ⌘A/C/X/V/Z, ⌥←/→/⌫, ⌘←/→/⌫, ⌃A/E/B/F/U and their Shift variants (selection). ⌘V pastes all of a multi-line field and the first line of a single-line one; the composer still looks for files and images first (R-1). Mouse: click places the caret, drag selects, double click selects a word, triple click a line, Shift-click extends. Under Vim mode the handler drops text while the field is in Normal or Visual mode; a one-line field scrolls sideways to keep the caret in view; a secret is drawn as dots and the element keeps only character offsets. The sidebar rename opens with the old name selected.

## Vim mode (V-1)

`editor.vim_mode` (default off, live; ⌘K → **Editor: Toggle Vim Mode**) gives Hutan's text inputs Vim's modal editing: the chat composer, the palette filter, prompt dialogs (not secrets), the ⌘O / ⌘J picker, the new-agent model filter, the settings sheet's search and value boxes, the sidebar filter and rename, the History and Resources filters, the Tasks form, the merge message and the attach form. Terminals are not text inputs: programs there keep every key.

- **Modes.** Fields open in **Insert** (typing as ever). Escape goes to **Normal** (the caret steps back onto the last character); in Normal mode Escape does what it always did (close the palette or dialog, clear the filter), so the second Escape behaves as without Vim. `v` starts a charwise **Visual** selection; Escape or `v` returns to Normal. A mode pill (`NORMAL` / `INSERT` / `VISUAL`) sits in the composer footer and at the right of a focused single-line field; the caret is a bar in Insert and a block over the character in Normal and Visual.
- **Normal.** Motions `h j k l`, `w b e`, `W B E`, `0 ^ $`, `gg G` (`3G` / `3gg` go to a line), `f t F T ; ,`, counts (`3w`). Operators `d c y` with motions, text objects `iw aw iW aW`, and doubled for lines (`dd cc yy`); `x X D C s S Y`, `r<c>`, `~`, `i a I A o O`, `p P`, `u` / `⌃R` (undo, redo), `.` (repeats the last change, typed text included). Words are runs of letters, digits and `_` (Unicode-aware: Cyrillic text works) or of other punctuation; `W B E` split on blanks only.
- **Visual.** Motions extend; `d x y c s ~` act on the selection; `o` swaps its ends; `iw` selects a word. Changes made in Visual mode are not repeated by `.`.
- **Registers.** One unnamed register. With `editor.vim_use_system_clipboard` (default on) yanks and deletes also write the system clipboard (a line with its newline) and `p` / `P` paste from it, lines staying lines; off, the register is private to the field.
- **Single-line fields.** `j k` do nothing, `dd` / `cc` take the whole text, `o` / `O` are `A` / `I`; pasted newlines become spaces. Enter in Normal mode is the field's confirm.
- **The composer.** Return and ⌘Return follow the send key (`chat.send_key`, § Send key) in Insert; in Normal mode Return is never a newline and sends only when the send key is `enter`; ⌘Return sends from any mode. A sent composer is back in Insert.
- **Undo** is a step per typed run and per Normal command (a `cw` with its typing is one step). ⌘Z / ⌘⇧Z work in the composer and the forms with or without Vim; the Vim-less path is unchanged otherwise.
- Not in scope: counts on `i`, marks, named registers, macros, `/` search, blockwise and linewise Visual.

## Send key (SK-1)

`chat.send_key` (`cmd-enter` default, or `enter`; Settings → Chat → *Send messages with*; the earlier boolean `chat.send_on_enter = true` still reads as `enter`, writes go to `send_key`) is the one rule for every place a *message* is written, read live by each of them:

- **Message composers**: the chat composer (harness chats, direct chats, Mawas Home), the Start sheet's prompt, the review comment box and "send review" in the Changes tray, and free-text answers in question cards.
- **`cmd-enter`**: Return adds a line, ⌘Return sends. **`enter`**: Return sends; ⇧Return and ⌥Return add a line; ⌘Return still sends (never a dead key). A Return that commits an input method's marked text never sends. In a question card, under `enter` Return submits from a multi-line field and from the last field, and moves on from any other one-line field.
- **Forms that are not messages keep their keys**: the merge and delivery commit messages (⌘Return confirms, Return is a newline), the memory editor (⌘Return saves), one-line fields (Return confirms or starts), the palette and pickers, and every settings field.
- **Quick switch in place**: the composer hint (`⌘↩ send · ↩ newline`, or `↩ send · ⇧↩ newline`) and the hint under the Start sheet's prompt are small controls; clicking one opens a menu of the two choices (⌘Return (Return adds a line) / Return (⇧Return adds a line)) that writes `chat.send_key`, and every open composer and hint updates at once. ⌘K **Chat: Toggle Send Key** flips it and shows "Return now sends" / "⌘Return now sends".
- **First-run choice**: § First-run setup (the Agents step's *Send messages with* line, else one line above the first focused composer; `chat.ask_send_key`).

## Keyboard defaults

`keymap.base` (Settings → `keymap`, default `zed`) picks the table the keymap starts from; `keymap.toml` (`[[binding]]` entries, `action = "none"` unbinds) is merged on top of either. Changing the base rebinds at once. The palette rows, the status bar tooltips and the empty-state hints show the keys of the base in effect.

The **Zed** base follows Zed's published macOS keymap (`default-macos.json`) wherever Hutan has the matching action, and keeps Hutan-only actions on keys Zed leaves free. The **Hutan** base is the table Hutan shipped before (kept for those who learned it).

| Action | Zed base | Hutan base |
|---|---|---|
| Command palette | ⌘⇧P (⌘K is Zed's chord prefix, not a palette key here) | ⌘K |
| Jump to any workspace, project or session, hidden ones included; `?` + words searches the session history | ⌘P (Zed's quick open) | ⌘J |
| Open a folder as a project (recent projects, Open Folder…) | ⌘O | ⌘O |
| Settings | ⌘, | ⌘, |
| New workspace | ⌘⇧N | — |
| Search the session history | ⌘⇧F | palette |
| New session (terminal) / new agent / new chat | ⌘T / ⌘⇧T / ⌘⌥T (Zed's ⌘⇧T reopens a closed tab, an action Hutan does not have) | same |
| Open / focus Mawas Home (a center tab; needs `assistant.enabled`) | none by default (palette → Mawas: Open; bind it in `keymap.toml` or set `assistant.global_shortcut`) | same |
| Previous / next tab (the focused dock's panels, else the active pane's tabs; wraps) | ⌘⇧[ / ⌘⇧] (also ⌃⇧Tab / ⌃Tab) | same |
| Tabs 1–9 in pane | ⌘1…⌘9 | same |
| Jump to next "needs you" | ⌘⇧J | ⌘⇧J |
| Toggle the Resources panel | ⌘⌥R (Zed's ⌘⌥R reruns a task; Hutan has no such action) | ⌘⌥R |
| Toggle left / right / bottom dock | ⌘B (Expanded ↔ Compact, a hidden sidebar comes back) / ⌘L (right panel open ↔ rail only) / ⌘J (also ⌃\`) | ⌘B / ⌘L / ⌘⌥J (the bottom dock has no rail: it opens or hides, tab strip and all); ⌘R and ⌘⌥B also toggle the right side in the Zed base, ⌘⌥B in the Hutan base |
| Hide the left sidebar / the right rail completely | palette → *View: Hide Left Sidebar* / *View: Hide Right Rail*, or the titlebar buttons' menus (the buttons stay and bring them back) | palette (no default key) |
| Close all docks | ⌘⌥Y | palette |
| Split right / down | ⌘D / ⌘⇧D, ⌘\\ (right) | ⌘D / ⌘⇧D |
| Split in a direction | ⌘K ←→↑↓ | palette |
| Focus the neighbor pane | ⌘K ⌘←→↑↓, ⌘⌥←→↑↓ | ⌘⌥←→↑↓ |
| Resize | ⌃⌘←→↑↓ | same |
| Zoom the pane | ⌘⇧↩, ⇧⎋ | ⌘⇧↩ |
| Terminal font size larger / smaller / reset (writes `terminal.font_size`) | ⌘= (or ⌘+) / ⌘- / ⌘0 | palette |
| Close tab (detach) / stop session | ⌘W / ⌘⇧W (Zed's ⌘⇧W closes the window; Hutan keeps stop) | same |
| Overview | ⌘⇧O | ⌘⇧O |
| Previous / next prompt (highlights the block) | ⌘↑ / ⌘↓ | same |
| Select / copy the current block's output | ⌘⇧A / ⌘⇧C | same |
| Approve / deny focused approval | ⌘↩ / ⎋ (only when the approval card has focus) | same |
| Send a message (chat, Start sheet prompt, review comment, question answer) | ⌘↩, or ↩ with `chat.send_key = "enter"` (⇧↩ / ⌥↩ then add a line) | same |

The block keys apply while the primary screen shows (a program on the alternate screen — vim, htop — hides blocks and gets every key). Inside a focused terminal every other key goes to the PTY; Option-as-Meta follows `terminal.option_as_meta`. Global keys (docks, palette, splits) work while a terminal has focus.

Chords (`keys = "cmd-k right"`) are GPUI multi-stroke bindings. A `keymap.toml` binding that is the prefix of another binding in the same context (for example `cmd-k` in the Zed base) is reported as a conflict.

Elsewhere in the docs "⌘K" names the command palette as the Hutan base binds it (⌘K → "…"); in the Zed base the palette is ⌘⇧P.

## Accessibility

- All chrome reachable by keyboard; visible focus rings (system style).
- State never conveyed by color alone: dot shape/fill + text subtitle.
- VoiceOver labels for panels, panes, session rows; terminal content exposed later (M6 a11y pass).
- Respect Reduce Motion, Increase Contrast.

## Open design questions

1. ~~App icon and name~~ → Hutan, orangutan mascot (`assets/brand/`); app icon `assets/brand/05-mascot/svg/icon-dark.svg`, mascot UX states listed in `assets/brand/README.md`.
2. Where approvals appear when the inspector is hidden: toast + sidebar only (current proposal).
3. Multi-window: one workspace per window vs any (proposal: any; presets per window).
