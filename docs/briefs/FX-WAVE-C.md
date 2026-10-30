# FX-WAVE-C — Markdown preview findings from the 2026-10-31 user test

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-WAVE-C`

**Contract ownership:** none (no protocol change expected).

## Context
Usability test of the 2026-10-30/31 wave on a bundle of master (report kept outside the repo). Findings to fix, verbatim:

### F6 (minor) Preview code blocks ignore the code font
Changelog: code font covers "previews' code". After setting `editor.font_family` = Courier New the source pane changed but the preview code block stayed in the old mono, also after closing and reopening the preview `set12c`, `set15`. Area: markdown preview / fonts.


### F10 (polish) Markdown preview gaps
`~~strike~~` shown literally; raw HTML (`<details>`) shown as text; SVG image has ~190 px blank above and below `md06/07`; broken image shows only alt text `md06`.


## Change
F6: preview code blocks follow `editor.font_family` live (FONT-PICKER-2's `Workbench::sync_fonts` / `FontOverrides`). F10: strikethrough, `<details>`/simple inline HTML handled sensibly (render `<details>` open with its summary as a heading-like line, other tags dropped to their text), SVG images sized by their viewBox without blank bands, broken image shows a clear placeholder with alt text.

## Acceptance
Each finding reproduced before the fix, a regression test where a consumer-visible behaviour is involved (GPUI tests: `common::click_at`, wait on state), Cua screenshots dark + light for the visible ones; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-WAVE-C)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxwavec/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
