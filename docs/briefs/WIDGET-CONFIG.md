# WIDGET-CONFIG — every widget can be moved, hidden, reordered and configured where it is

Agent: implementer (Claude Sonnet 5.5)   Depends on: FX-STATUSBAR, SB-ATTACH merged   Branch: `slice/WIDGET-CONFIG`

**Contract ownership:** none.

## Context
The user (2026-10-22): "each widget configurable". ADR 0010 already makes every UI feature a registered panel / pane item / status item with its own settings namespace, and presets can name docks and status items (`docs/design.md` § Workbench model, the `[layout.presets.*]` example). What is missing is doing it **in the UI**, where the widget is, without editing `settings.toml`. Decision (orchestrator; the user did not answer which part matters more): both — placement and per-widget options.

## Items (in commit order)
1. **Panel menu** (⋯ in every panel header, and right-click on its rail icon / dock tab): *Move to ▸ Left · Right · Bottom · Center tab*, *Hide*, *Panel settings…* — the latter opens an inline popover with that panel's own namespace (`panels.<id>.*`) rendered by the same rows as the Settings sheet, plus *Open in Settings*. Every registered panel gets it; panels with no options show only placement.
2. **Rail and docks**: right-click the right rail or a dock's tab strip → a checklist of available panels (show/hide), drag rail icons to reorder; changes are written to the active preset (or create *Custom* from a built-in, never edit a built-in in place — say so in one line).
3. **Status bar**: right-click → checklist of status items (left/right groups), drag to reorder; each item's own options (e.g. `workspace`: show branch; `connection`: show version) in its context menu.
4. **Sidebar sections**: the left sidebar's sections (containers list, nav rows, Memory, Tasks) can be hidden/reordered from its ⋯; *Reset layout to preset* everywhere it applies.
5. **Persistence and undo**: every change goes to `settings.toml` through the existing layered settings writer (provenance *You*), takes effect live, and is undoable with ⌘Z for 10 s via a toast (*Moved Files to the left · Undo*).
6. Docs: `docs/design.md` (§ Customizing the workbench), `docs/themes.md` if tokens change.

## Acceptance
GPUI tests: move each kind of widget between docks/center and back; hide/show from the rail and status bar menus; reorder persists after restart; a built-in preset is never modified (a *Custom* preset appears); Panel settings popover edits a `panels.<id>` key and the panel updates live; Undo toast restores. Cua screenshots of the menus and the popover, dark + light → `~/hutan-scratch/widgetconfig/shots/`. `just check` green; commit per item, messages end `(WIDGET-CONFIG)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
