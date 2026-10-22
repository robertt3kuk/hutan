# UX-BUTTONS — Buttons by the book: one button system, every surface migrated

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UX-BUTTONS`

**Contract ownership:** none (stop and report if a wire change seems needed).

## Context
The user (2026-10-22): "make all buttons per best UI/UX practices, prettier style". Evidence and numbers: `docs/spikes/2026-10-22-ui-polish/buttons.md` (hierarchy, 28 px default / 24 px compact, 28 px icon hit region with 16 px glyph, states: hover/pressed/focus-visible/disabled 0.45/loading spinner in place of the leading icon, verb-first sentence-case labels, primary trailing and Cancel leading, Return = default, Esc = cancel, destructive never default) and the clean-aesthetic rules (one primary per surface, ghost secondary, radius 6). Existing primitives: `crates/desktop/src/ui/` (CL-PRIMITIVES).

## Items (in commit order)
1. **The system**: `ui::Button` with variants Primary / Secondary / Ghost / Destructive / Link and `IconButton`; sizes Default 28 / Compact 24; states hover (+surface overlay), pressed, focus-visible ring (keyboard only), disabled (0.45, default cursor, no hover), loading (spinner replaces leading icon, width fixed, not clickable); optional shortcut hint; tooltip required for icon-only. Tokens in the theme (`button.*`), documented in `docs/themes.md`.
2. **Audit**: list every button-like element in `crates/desktop/src` and `crates/chat_view`, `crates/code_view` (grep for click handlers on `div`s styled as buttons) into `docs/spikes/2026-10-22-ui-polish/buttons-audit.md` with surface, current look, target variant.
3. **Migrate** surface by surface (one commit each): sheets/dialogs (Start, Rewind, Settings, Setup, confirms — primary trailing, Cancel leading, Return/Esc, destructive confirms never default), chat (composer send/stop, approval card, queue chips, turn footer), editor bars (agent bar, conflict, trust), panels (Changes/Finish, Files, Search, Problems, Resources, Tasks), sidebars and title bar icon buttons, empty states. Labels verb-first sentence case (no "OK").
4. Docs: `docs/design.md` § Buttons (the table of variants and where each is used).

## Acceptance
Unit/GPUI tests for states (disabled not clickable, loading not clickable and keeps width, focus ring only after keyboard focus, Return triggers the default, Esc the cancel in every sheet); grep test: no ad-hoc button styling left outside `ui::Button` (allow-list for the few true exceptions, each with a comment). Screenshots before/after for each migrated surface (dark + light).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks), `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md`. Commit after every item (messages end `(UX-BUTTONS)`); update existing tests to the new spec keeping what they prove; Cua screenshots dark + light before/after into `~/hutan-scratch/uxbuttons/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
