# MEM-SIDEBAR — Mawas memory lives in the sidebar, not in the center

Agent: implementer (Claude Sonnet 5.5)   Depends on: —   Branch: `slice/MEM-SIDEBAR`

**Contract ownership:** none.

## Context
The user (2026-10-22): "Mawas's memory should be in the sidebar too, not in the center pane". Evidence: `docs/spikes/2026-10-22-ui-polish/orca-sidebar.md` Part 3: `mawas-memory` is registered with `default_dock: Left` (`panels/mod.rs:246-252`) but `open_panel` routes it to the center (`view.rs:1757-1759`); `is_mawas_page` (`actions.rs:859-862`), `open_memory` / `memory_panel` (`assistant.rs:82-108`), `panels/memory.rs:1-12` ("a tab of the center, one instance"); Orca swaps the left sidebar body in place (`sidebarBody`). Memory today: `docs/assistant.md` § Memory (curated entries, Global/Workspace scope, proposals waiting for approval, editor card).

## Items (in commit order)
1. **Routing**: `mawas-memory` opens in the left sidebar as a body that replaces the Projects body (Orca's body switch), with a back affordance (`‹ Projects` header like the Files page) — from the *Memory ›* link in Mawas Today, ⌘K *Mawas: Memory*, a **Memory** nav row under Mawas (shown when Mawas is enabled). Remove the center-tab path; a saved layout with a Memory center tab migrates to the sidebar body (once, logged).
2. **Fit the sidebar width (240–480 px)**: one column — scope filter (*All · Global · <active workspace>*, following SB-ATTACH's active container when present, else the shown workspace), *Waiting for you* proposals first (compact cards: what, where, Approve / Edit / Reject as `ui::Button`s), then entries grouped by category with one-line rows (text truncated, full text on hover/selection); selecting an entry opens its editor inline under the row (not a modal); **Add** in the header. Empty state one sentence.
3. **Keyboard**: ↑↓ entries, Return edits, ⌘Return saves, Esc closes the editor, ⌘⌫ archives (confirm), Tab moves between proposals and entries.
4. Docs: `docs/assistant.md` § Memory, `docs/design.md` (left sidebar bodies; Memory removed from center tabs).

## Acceptance
GPUI tests: every entry point opens Memory in the sidebar, never a center tab; a saved layout with the old center tab migrates; proposals approve/reject from the sidebar; edit inline and save; narrow (240 px) and wide (480 px) layouts without overlap (laid-out bounds check like FX-TABS-RES). Cua screenshots dark + light → `~/hutan-scratch/memsidebar/shots/`. `just check` green; commit per item, messages end `(MEM-SIDEBAR)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
