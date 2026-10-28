# UI-RHYTHM — spacing scale, nicer settings with nav icons, quit sheet

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UI-RHYTHM`

**Contract ownership:** none (desktop + settings icon ids; no protocol).

## Context
User 2026-10-28: "settings should look nicer — spacing and the rest, by examples; settings with icons"; the quit sheet looks rough. Study `docs/spikes/2026-10-28-ui-study/README.md` §2.3, §2.4, §3 gaps (G1–G11 with measurements), mocks `mock-settings.svg`, `mock-quit.svg`; plan rows 1 (`FX-QUIT-SHEET`) and 2 (`UI-RHYTHM`). Decisions taken by the orchestrator on the study's §6 (user may overrule): quit sheet primary = **Keep running** (Stop is the danger option); harness glyphs = **our own monochrome monograms** (π OMP, asterisk Claude Code, prompt-in-ring Codex), user packs can replace them; usage meters in the status bar **on by default** once a harness reports a limit; hooks stay opt-in, the first agent card says what they add; two-line cards for **agents only**.

## Items (in commit order)
1. **FX-QUIT-SHEET**: `modal.rs` button row right-aligned, wraps/stacks under 480 pt, never overflows (every modal with ≥ 3 buttons); the quit sheet per `mock-quit.svg` (short wording, the live sessions listed, *Keep running* primary, *Stop sessions and quit* danger, Cancel).
2. **Spacing scale** tokens (4·8·12·16·24·32) and shared pieces (`ui::sheet`, `ui::card_section`, `ui::row_fixed`) per plan row 2.
3. **Settings look**: fixed row height 52 pt, one-line help with the rest in a tooltip, one aligned control column, page title 20 + description, section cards; **nav icons** for every page (new `IconId`s from the pinned icon pack, schema + `docs/themes.md`); keep every page id, ⌘K and `hutan ui set` unchanged (the information-architecture regroup is a later slice, SETTINGS-IA).
4. Docs: `docs/design.md` (spacing scale, settings, dialogs).

## Acceptance
GPUI tests: quit sheet at 400/600/900 pt widths (no overflow, button order, default), settings row geometry and nav icons, page ids unchanged; Cua screenshots before/after (dark + light) into `~/hutan-scratch/uirhythm/shots/`; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(UI-RHYTHM)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/uirhythm/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
