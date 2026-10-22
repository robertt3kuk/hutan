# UX-MOTION — Smooth motion: tokens, the places that move, Reduce Motion done right

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UX-MOTION`

**Contract ownership:** none (stop and report if a wire change seems needed).

## Context
The user (2026-10-22): "animations — just smooth". Evidence and the GPUI API limits: `docs/spikes/2026-10-22-ui-polish/motion.md` (tokens 100 / 150 / 200 / 250 ms; ease-out quint-like curve for enter and exit, drawer curve for sheets, never ease-in, exit shorter than enter; never animate keyboard actions, typing, scrolling content, terminal output, theme switches, focus ring; GPUI `with_animation` gives opacity and layout, no transform; `with_spring` for reversible; Reduce Motion currently read once through `defaults` and snaps `with_animation` to the end). Clean-aesthetic cap 150 ms except sheets/toasts 200 ms.

## Items (in commit order)
1. **Motion tokens and helpers**: `ui::motion` with durations, easing functions (add the cubic-bezier helper GPUI lacks), and helpers `fade_in/out`, `expand/collapse` (height), `slide_fade` (offset via layout margin where transform is missing) — all no-ops under Reduce Motion except ≤ 150 ms opacity.
2. **Reduce Motion live**: read `NSWorkspace.accessibilityDisplayShouldReduceMotion` and observe `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`; replace the `defaults` read; settings override `appearance.reduce_motion = system|on|off`.
3. **Apply**: hover/pressed colour fades on buttons and rows (100 ms), popovers/menus/pickers (fade + 4 px slide, 150 ms in / 100 ms out), sheets (fade + slide, 200 ms in / 150 ms out), toasts (200/150), collapse/expand of sidebar groups, tool cards, Finished group (150 ms height), queue chips and approval card appear/leave (150), tab close (100 fade). Interruptible: reversing mid-flight continues from the current value.
4. **Performance guard**: a test that animations do not request frames when idle (B3 idle CPU unchanged) and that typing in the editor/composer never waits on an animation; `docs/perf.md` note.
5. Docs: `docs/design.md` § Motion (table: what moves, duration, curve, Reduce Motion behaviour).

## Acceptance
GPUI tests with the test clock: each applied motion reaches its end state at the token duration; Reduce Motion on → no movement, opacity ≤ 150 ms; idle window requests no frames; keystroke path unchanged (B4/B12a numbers not worse). Cua: short screen recordings or frame sequences of popover, sheet, toast (dark), plus Reduce Motion on.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks), `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md`. Commit after every item (messages end `(UX-MOTION)`); update existing tests to the new spec keeping what they prove; Cua screenshots dark + light before/after into `~/hutan-scratch/uxmotion/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
