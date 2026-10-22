# FX-E2E4 — fixes from the real-window pass over the UI wave

Agent: implementer (Claude Sonnet 5.5)   Depends on: WIDGET-CONFIG merged   Branch: `slice/FX-E2E4`

**Contract ownership:** none.

## Context
`docs/spikes/2026-10-23-e2e-usertest-4.md` (findings F1–F11, polish list, Top 10; shots in `~/hutan-scratch/e2e4/shots/` — open the ones each finding cites).

## Items (in commit order)
1. **F1 — popovers above splitters**: popovers and menus must take hit-tests and hover over dock/pane resize handles (the daemon popover's *Start* was dead where it overlapped the bottom dock's edge). Fix generally (overlay layer order / handles inert under an open overlay), GPUI test: click the middle of a popover button that overlaps a dock edge.
2. **F2, F3 — stopped daemon**: one message, not four (status bar `○ Stopped · Start`, popover, banner, card): keep the status item + one popover; clicking `Stopped · Start` starts the daemon directly (no second click); never show "hutand restarted" for a stop by the user or an Undo.
3. **F5 — Undo of Close Split** puts the session back into its split (same position/size), not a new tab, and shows no restart card.
4. **F6 — toasts**: the text never changes after it appears ("asks to edit: src/lib.rs" stays), and an Undo toast keeps its height (no one-line/two-line jump).
5. **F7, F8 — dots**: the unread dot is ≥ 6 px and centred on the row's text line; a tab's attention dot is never clipped (keep it inside the tab's padding; scroll the tab into view when it gets one).
6. **F9 — zoomed split** shows a cue (the tab's `⊞ N` becomes `⊞ N · zoomed` or a small zoom glyph; the hidden splits' count stays).
7. **F10, F11 — tooltips and branch**: rail tooltips follow reorders; the status bar's branch follows the visible tab's checkout consistently.
8. **F4 — rail checklist**: clear check marks, the dock's order, and *Reset to built-in layout* (not "Reset layout to Custom").
9. **Polish list**: de-duplicate labels (Search ×3, Files project ×2), one-line session rows, a session's age from its last activity.
10. **Prose face**: the serif face in empty states and Mawas prose → the UI face (one family app-wide except code), unless `docs/design.md` names a reason; update the type scale docs.

## Acceptance
GPUI tests for 1–8 (behaviour); before/after Cua screenshots dark + light for 2, 4, 5, 6, 8, 9, 10 → `~/hutan-scratch/fxe2e4/shots/`. `just check` green; commit per item, messages end `(FX-E2E4)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
