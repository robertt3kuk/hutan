# TERM-SPLITS — terminal splits like Ghostty and iTerm: several terminals inside one tab

Agent: implementer (Claude Sonnet 5.5)   Depends on: —   Branch: `slice/TERM-SPLITS`

**Contract ownership:** none expected (splits are layout state in the desktop; each split is its own daemon terminal session). Stop and report if a wire change seems needed.

## Context
The user (2026-10-22): "terminal splits please". Today ⌘D / ⌘⇧D split the **pane** (a new pane with its own tab strip) and start a new terminal in the focused terminal's cwd (`docs/design.md` § Workbench model keys, lines ~51, 883–886). Terminal users expect the Ghostty / iTerm2 / Warp / WezTerm model: **one tab holds a tree of terminal splits**, no tab strip per split, the tab closes when its last split closes. Decision (orchestrator, the user did not answer the style question): implement that model for terminals; pane splits stay for mixing kinds (file + terminal, chat + terminal).

## Items (in commit order)
1. **Model**: a terminal tab item can hold a split tree of terminal sessions (`ItemRef::TerminalSplit { tree }` or an extension of the terminal item) persisted with the layout; each leaf is a daemon session; layouts from before load unchanged.
2. **Keys** (both keymap bases; documented): with focus in a terminal — ⌘D split right, ⌘⇧D split down (new terminal in the focused split's cwd from OSC 7), ⌘⌥←→↑↓ focus the neighbour split, ⌃⌘←→↑↓ resize, ⌘⇧↩ zoom the split (toggle), ⌘W closes the focused split (the session ends per the existing close rules: confirm when a process runs), ⌘⇧W closes the whole tab. Pane splits keep their palette commands and ⌘K arrows (Zed base). Outside terminals ⌘D keeps its current meaning (file tab: select next occurrence).
3. **Look**: 1 px hairline dividers (`border`), the focused split's divider side or a 2 px top edge in `accent` at low opacity (not colour only: unfocused splits dim text 8 %, configurable `terminal.dim_unfocused_splits`), drag dividers to resize (min 6 cols / 3 rows), double-click a divider to equalise. The tab title is the focused split's title; a small `⊞ 3` count in the tab when it holds splits.
4. **Mouse and menus**: terminal context menu *Split Right / Split Down / Close Split*; drag a terminal tab onto another terminal's edge to merge it as a split (drop zones as for panes); *Move Split to New Tab*.
5. **Sessions and attention**: each split is a session in the sidebar under the tab's container; attention/requests of any split show on the tab (dot); the recovery card after a daemon restart works per split.
6. Docs: `docs/design.md` (terminal splits vs pane splits, keys), `docs/shell-integration.md` (cwd for new splits).

## Acceptance
GPUI tests: ⌘D / ⌘⇧D create splits inside the tab with the right cwd; focus/resize/zoom/close keys; last split closes the tab; layout round-trip after app restart (splits restored, sessions reattached); drag-to-merge and move-to-tab; ⌘D in a file tab unchanged. Cua screenshots: a tab with 3 splits dark + light → `~/hutan-scratch/termsplits/shots/`. B4 keystroke budget unchanged in the focused split. `just check` green; commit per item, messages end `(TERM-SPLITS)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
