# QUICK-TERMINAL — a quick terminal on a global hotkey

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/QUICK-TERMINAL`

**Contract ownership:** none expected.

## Context
Ghostty #2384 (107 reactions, 55 comments), #4624, cmux #2758/#2411 — a terminal that drops down over everything on a global hotkey (`docs/spikes/2026-10-25-user-needs/terminals.md`). Hutan already registers one global shortcut (Mawas, Carbon hot-key path, `docs/design.md` § global shortcut).

## Items (in commit order)
1. **Global hotkey** `terminal.quick_hotkey` (default ⌥Space? check conflicts with macOS/Spotlight/Raycast defaults and choose an unbound default such as ⌃` ; configurable; off switch) shows a borderless panel window sliding down from the top of the active screen (Reduce Motion: fade), over full-screen apps where macOS allows, 40 % height (configurable), on the screen with the pointer.
2. **The terminal** is a normal Hutan session (in the Home folder or the last active project — setting), kept between shows (hide, don't kill); Escape or the hotkey hides it; ⌘T adds tabs inside it; ⌘⇧↩ moves it into the main window as a tab.
3. **Focus**: showing it takes key focus and returns it to the previous app on hide; it never steals focus when another app is in secure input.
4. Docs: `docs/design.md`, Settings → Terminal rows.

## Acceptance
GPUI tests where possible (panel model: show/hide toggles, session kept, move to main window); unit test for hotkey parsing/conflict check; Cua screenshots of the panel over another app (dark/light) — never over the user's own Hutan window; manual note on full-screen behaviour.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(QUICK-TERMINAL)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/quickterminal/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
