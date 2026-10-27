# FX-WORK-1 — workability fixes: palette, scroll chains, ⌘P, chat, terminal keys

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-WORK-1`

**Contract ownership:** none (desktop; no protocol).

## Context
Fixes from the workability evaluation `docs/spikes/2026-10-27-workability.md` (user 2026-10-27: "evaluate how workable it is"; screenshots in `~/hutan-scratch/e2e6/shots/`, numbers per finding). Stay out of the Changes view, the Files panel and `code_view` (other slices own them).

## Items (in commit order; each with a regression test)
1. **F-08 palette scrolling**: the wheel/trackpad scrolls the palette list (not the editor behind it); events over any popover/sheet never reach what is under it (fix at the overlay layer so every popover benefits); the palette shows as many rows as fit (cap by window height, not 12).
2. **F-15 nested scroll chains**: scrolling a scrollable list inside a sheet (Start sheet model menu) stops at its end and does not scroll the sheet (no scroll chaining), app-wide rule in the shared scroll container.
3. **F-13 ⌘P with spaces**: words separated by spaces are AND-matched (`searcher glue` finds `…/searcher/glue.rs`), like Zed/VS Code.
4. **F-12 false "another live agent works in this checkout"**: only sessions with a running or waiting agent turn count (an idle terminal and an empty Mawas session do not); fix the predicate in one place.
5. **F-14 chat line breaks**: single newlines in assistant answers are kept as line breaks (render as GitHub-flavoured Markdown "breaks" mode for assistant text; code blocks unchanged).
6. **F-16 terminal keys**: ⌘↑/⌘↓ in a focused terminal scroll to top/bottom of its scrollback (iTerm/Terminal.app) instead of leaving the terminal; check the keymap for the conflicting binding and move the workbench action off it inside terminals.
7. **F-17 project replace**: after *Replace All*, the notice and the replacement text clear when the query changes.
8. Docs: `docs/design.md` (rows for each behaviour changed).

## Acceptance
GPUI tests per item (wheel events over the palette and a sheet list, ⌘P query with a space, the predicate with idle sessions, chat render of single newlines, ⌘↑ in a terminal, replace notice cleared); Cua screenshots of the palette scroll and the chat newlines (dark + light) into `~/hutan-scratch/fxwork1/shots/`; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-WORK-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxwork1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
