# CL-POLISH-4 — structural visual fixes from the style review (top 9–15)

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-BUTTONS, UX-MOTION merged   Branch: `slice/CL-POLISH-4`

**Contract ownership:** none.

## Context
The user (2026-10-22): "make the style prettier". A senior-design review of every surface, dark and light, wide and narrow: `docs/spikes/2026-10-22-ui-polish/style-review.md` (per-surface fixes with exact values, and the ranked top 15; shots in `~/hutan-scratch/style/shots/` and `pairs/` — look at them). Rules: `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md` § Buttons, § Motion, § Visual system.

## Items (in commit order; the number is the review's rank)
9. Remove duplicate headings (Changes, Requests, Problems, Files, History: the dock tab already names the panel).
10. Start sheet model picker as a popover list grouped by family, pre-scrolled to the current model (the same list as Switch model; reuse FX-E2E2-B's ordering and current mark); fix: after a click-select, Return closes the picker.
11. Changes toolbar: one segmented control, icon Refresh, drop `updated just now`; unstaged glyph instead of a pill.
12. Chat header: no pill fills for Mode/context; no repeated per-turn context line; 24 pt top inset.
13. Question card spacing (8/16 px), one Decline, radios on the left; Requests panel buttons as 2×2.
14. Settings: descriptions ≤ 70 characters or wrapped; control column min-width 120; footer only when there are problems.
15. ⌘K opens the command palette outside terminals (today it is a chord prefix — move the chords that start with ⌘K behind it or document the change in the keymap tables); palette ranking: shortcut/prefix matches first (`new` → `Session: New Terminal` near the top). Also look at: a stray "Requests" menu over Resources in a narrow window (review § narrow).

## Acceptance
Each item: the fix as specified in the review (or a better-argued variant, said in the commit), existing tests updated to the new spec keeping what they prove, a GPUI test where it is behaviour (focus, ranking, Return, title length), and before/after Cua screenshots dark + light into `~/hutan-scratch/clpolish4/shots/`. Contrast numbers for any colour you change (WCAG AA for text). `just check` green; commit per item, messages end `(CL-POLISH-4)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
