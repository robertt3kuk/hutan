# FX-POLISH-1 — launcher, chat queue, agent bar, terminal links: the e2e pass's polish list

Agent: implementer (Claude Sonnet 5.5)   Depends on: UX-QUEUE-STEER, E-10, CL-AGENTS merged   Branch: `slice/FX-POLISH-1`

**Contract ownership:** none.

## Context
From `docs/spikes/2026-10-23-e2e-usertest.md` (top issues 7, 9, 10; B5–B7; friction). Screenshots referenced there are in `~/hutan-scratch/e2e/shots/` (read them). Follow `docs/spikes/2026-10-21-clean-aesthetic.md` (quiet workbench) and `docs/process.md` § UI checks.

## Scope (each item: fix, test where it is behaviour, screenshot before/after)
1. **Launcher / first-run composer** (shots 05–08, 70): switching *What* to Plain terminal must not make the form jump (reserve or animate-free collapse with the primary button anchored); for a terminal hide the task-only fields (Title placeholder "What it is called in the Tasks panel…", *Queue as task / Starts the task now*); with Options open the primary button stays visible (scroll the options, not the button); the shared-checkout warning wraps instead of clipping; the Open panel starts at the active project's root.
2. **Status bar overflow** (shot 71): the "N other live agent(s) already work in …" sentence moves out of the status bar into the launcher/session warning; the status bar shows at most a short item.
3. **Chat queue** (shots 54, 57, 63b): a queued chip's button says *Send now* when no turn runs (not *Interrupt & send*); × buttons align with their rows' text baseline; rows use the neutral surface, not the orange tint; ⌃↩ *Interrupt & send* shows *Stopping…* like the Stop menu does.
4. **Agent bar and conflict bar** (shots 75, 80, 81, 87): the agent bar names the agent (harness + session name), not the chat title, and has no stray period; the conflict bar says who changed the file ("Claude Code changed store.rs on disk"; "changed on disk" when unknown) and its diff has a legend (yours / on disk); Accept All must not shift the text (keep the scroll anchor); Reload on the conflict bar says "⌘Z brings your text back" once.
5. **Terminal links** (shots 10, 11): hovering a detected `path:line` shows a subtle underline and a tooltip "⌘-click to open"; the block hover toolbar must not cover the command text (place it at the block's top-right over the output area, or inset the command line).
6. **Chat header / rows**: raw model ids (`gitlab-duo/duo-chat-opus-4-6`) shown as the provider's display name when known, else the id's last segment with the full id in a tooltip; the cryptic `⚠ main` pill on session rows becomes the words "shared checkout" with a tooltip.
7. **Empty centre** (shot 25): after closing the last tab the centre shows the quiet empty state (`ui::EmptyState`, one sentence + ⌘P / ⌘T hints), not a blank pane titled "Hutan".
8. **Nav layout shift** (shots 05→08): the *Tasks* nav row appearing after the first terminal must not shift the nav (reserve or insert below without moving the selected row).
9. **Settings row provenance** clipped to "You" → full "Set by you".

## Acceptance
Each item has a before/after Cua screenshot (dark; light for 1, 3, 4) in `~/hutan-scratch/fxpolish1/shots/`; behaviour items (1 jump, 3 button label, 4 scroll anchor, 5 tooltip, 7 empty centre) have GPUI tests. Existing tests pinning old wording/visuals are updated to the new spec keeping what they prove. `just check` green; commit per item, messages end `(FX-POLISH-1)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
None.

## Out of scope
Needs-you/review semantics and Finish message (FX-REVIEW), Vim (FX-VIM-1), ⌘P files (E-13a); `docs/status.md`, `docs/roadmap.md`.
