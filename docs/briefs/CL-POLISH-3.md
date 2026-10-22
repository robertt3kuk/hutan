# CL-POLISH-3 — quick visual fixes from the style review (top 1–8)

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-BUTTONS, UX-MOTION merged   Branch: `slice/CL-POLISH-3`

**Contract ownership:** none.

## Context
The user (2026-10-22): "make the style prettier". A senior-design review of every surface, dark and light, wide and narrow: `docs/spikes/2026-10-22-ui-polish/style-review.md` (per-surface fixes with exact values, and the ranked top 15; shots in `~/hutan-scratch/style/shots/` and `pairs/` — look at them). Rules: `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md` § Buttons, § Motion, § Visual system.

## Items (in commit order; the number is the review's rank)
1. One selection fill per list (no fill on the parent project row of a selected child; no second selected nav row).
2. Light-mode cards: border 1 px @ 6 % (or the `surface` tone) for tool cards, user bubble, toasts; toast shadow 18 %.
3. Short chat titles: first clause, ≤ 40 characters (full prompt in the tooltip); the window title bar never repeats the active tab's title.
4. Toasts placed off the sidebar toolbar (bottom-right of the center, above the status bar); `Undo` as a link-weight action.
5. Selection bars inset to the page padding (Agents page, Search results): 16 px, radius 6.
6. Pills: neutral for `shared checkout` / `unstaged` / state words; warning only as a 16 px glyph; rows keep their one-line height.
7. One primary per surface in the setup flow (`Install…`, `Open a folder`) and while Finish is open in the Changes view.
8. Composer glyphs: send = ↑ arrow, stop = ■ square; disabled send = neutral fill.

## Acceptance
Each item: the fix as specified in the review (or a better-argued variant, said in the commit), existing tests updated to the new spec keeping what they prove, a GPUI test where it is behaviour (focus, ranking, Return, title length), and before/after Cua screenshots dark + light into `~/hutan-scratch/clpolish3/shots/`. Contrast numbers for any colour you change (WCAG AA for text). `just check` green; commit per item, messages end `(CL-POLISH-3)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
