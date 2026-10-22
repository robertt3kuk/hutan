# UX-MOTION-2 — exits that animate, chips that leave, rows that settle

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-MOTION, UX-BUTTONS merged   Branch: `slice/UX-MOTION-2`

**Contract ownership:** none.

## Context
UX-MOTION (`docs/design.md` § Motion, `crates/desktop/src/workbench/ui/motion.rs`: tokens, `Transition`, `present`/`Presence`/`Presences`, `Ghost`, `expand`) left these gaps (its report): pickers, sheets and the ⌘K/⌘P palette **leave instantly** because their state is dropped on close; queue chips leave instantly; collapse is used only in the Start sheet. The user asked for motion that is "just smooth" everywhere it belongs. Evidence: `docs/spikes/2026-10-22-ui-polish/motion.md`.

## Items (in commit order)
1. **Exit via `Ghost`** for every overlay that closes: palette (⌘K/⌘P/⌘J), pickers (model, theme, icon, scope, backend), Settings / Start / Setup / Rewind / Checkpoints sheets and the modal — exit 100–150 ms per the table, interruptible (reopening mid-exit continues from the current value), never delays focus: the keyboard goes back to its owner immediately on close.
2. **Chips and cards that leave**: queue chips and approval cards fade/collapse out (150 ms) when sent/answered; the answered approval collapses into its one-line summary.
3. **Collapse/expand** with `expand` where it is not inside a virtualized list: sidebar section disclosure (Requests / Failed / Ready for review / Finished groups if not virtualized), Settings "Advanced ▸", tool-card details in the chat (only if the chat list is not virtualized at that level — if it is, document why not).
4. **Reduce Motion** respected by all of the above (no movement, fades ≤ 150 ms), checked with the same test helpers.
5. Docs: `docs/design.md` § Motion table updated.

## Acceptance
GPUI tests on the test clock: each exit reaches its end at its token; reopening mid-exit is continuous; focus returns at close time (assert focus before the exit ends); chips/cards leave with motion and the list does not jump after the exit; Reduce Motion: no movement. Idle frame guard unchanged. Cua frame sequence of palette close and a chip leaving (dark). `just check` green; commit per item, messages end `(UX-MOTION-2)`. Fresh `HUTAN_HOME`.

## Out of scope
Virtualized lists' insert/remove animation; `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
