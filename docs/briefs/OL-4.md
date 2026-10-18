# OL-4 — Orca-style polish after the integrator's visual pass

Owner: implementer. Crate: `desktop` (+ `assets/`). No protocol/daemon changes. Context: `docs/briefs/OL.md`, `docs/spikes/2026-10-18-orca-layout.md` (§4 visual language: sizes/tokens), Orca source `/Users/workie/proj/orca-src-git` (look only; MIT).

Screenshots of the current master (integrator, bounded driver; 1280×800 window): `/Users/workie/proj/hutan-usertest/scratch/ol-p0.png` (default), `ol-p1.png` (Tasks page), `ol-p2.png` (Agents page), `ol-p3.png` (Assistant page), `ol3-1.png`/`ol3-3.png` (History, row expanded). Read them.

## Fix

1. **Window title** follows the active **center** item (session/tab title), never the focused left/right page ("History", "Tasks", "Overview", "Assistant" must not become the window title).
2. **Left nav rows** to Orca's metrics: outer padding 8 px, gap 2 px, row padding 8 px × 6 px, 13 px medium label, 16 px icon (≈30 px rows; now ≈40 px). Muted inactive icon/label, `sidebar_accent` selected fill, hover 8 % fill. Projects section header 32 px, 12 px semibold muted.
3. **Real icons.** Add a small set of Lucide SVG icons (ISC license — add `assets/icons/LICENSE-lucide.txt` and a note in `deny.toml`/README if needed; vendor only the SVGs used) loaded through a GPUI `AssetSource` and drawn with `svg()` tinted by theme colour: nav (search, list-checks, layout-dashboard, sparkles), right strip (history, git-compare/diff, inspect/info, git-branch-ish for agents, gauge for resources), History header (rows/group, filter, refresh, settings), row actions (chevron-down/up, more-horizontal), Projects header (+, sort, settings), bottom toolbar (plus, settings). Replace the Unicode glyphs there. Bundle: make sure `xtask bundle` includes them (embed with `rust-embed`-style `include_bytes!` is fine — prefer compile-time embedding, no runtime file paths).
4. **Agents = Agent Dashboard drawer** like Orca: the Agents nav item opens a non-modal drawer over the center area (from the left sidebar's right edge, below the 36 px titlebar, above the status bar, width min(remaining, 1294 px)) with a kanban: **Needs you · Working · Done · Idle** columns (min 264 px, 11 px uppercase headers with count pills, cards 8 px apart: session title, agent, last message, age; click opens/focuses the session and closes the drawer). Escape or clicking the nav item again closes it. The left sidebar keeps the Projects list while the drawer is open. Badge on the nav row = needs-you count (exists). Remove the cramped Overview-in-sidebar page (the Overview preset/center board can stay as is for ⌘⇧O).
5. **Assistant left page at 220–500 px:** one compact header row — scope as a small dropdown (Global / this workspace), then a backend·model chip that truncates with `…` and opens the model/backend menu (Switch model inside it); state dot. Nothing may clip at 280 px. The empty-state text and composer fit the width.
6. **History panel:** rows keep 12 px right padding (preview text currently touches the edge), the latest-turn preview clamps to 2 lines **with an ellipsis**, the title to 1 line with ellipsis; the expanded row uses a subtle `right_sidebar_accent` background, not the orange accent tint; segmented control selection uses the neutral accent surface (Orca style), orange only for the focus ring.
7. **No clipped text anywhere in both sidebars**: long folder/project/session names, chips and buttons truncate with `…` (audit the Projects tree, Tasks page chips, Changes tab rows, Requests).

## Tests

GPUI: window title follows the center item while a sidebar page is focused; nav row height within 28–32 px; Agents drawer opens/closes (Escape, nav click), shows sessions in the right column by state, clicking a card focuses that session; assistant header at 280 px has no element wider than the sidebar; history row preview ends with … when long; icons render (asset lookup succeeds for every icon name used — one test iterating the icon enum). Update `docs/design.md`.

## Acceptance

`just check` green, one commit on `slice/OL-4`. Do not edit `core`/`protocol`/`daemon`, `docs/status.md`, `docs/roadmap.md`.
