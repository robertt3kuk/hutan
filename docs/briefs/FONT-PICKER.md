# FONT-PICKER — pick fonts from a list

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FONT-PICKER`

**Contract ownership:** none (desktop; settings; no protocol).

## Context
User 2026-10-30 (after updating to 0.3.4 in the app — it worked): "fonts — we should be able to load them and show them as a list (Mac for now), to choose rather than type".

## Items (in commit order; each with a regression test)
1. Enumerate installed font families on macOS (CoreText `CTFontManagerCopyAvailableFontFamilyNames` / NSFontManager through `objc2`, off the UI thread, cached, refreshed when fonts change — `kCTFontManagerRegisteredFontsChangedNotification`); mark monospaced families (`kCTFontMonoSpaceTrait`).
2. Every font setting (UI font, editor/buffer font, terminal font, chat font if any — find them in `crates/settings`) becomes a **searchable picker** in Settings: each family drawn in its own face, monospaced-only by default for editor/terminal (toggle *Show all*), the current value first, *System default* entry, a preview line; typing a family name not installed still works and shows a warning.
3. *Install font…* opens Font Book / a file picker to add a `.ttf/.otf` (via `CTFontManagerRegisterFontURLs` user scope) — only if cheap; otherwise a link to Font Book.
4. Docs `docs/design.md` (Settings), `docs/themes.md` if fonts are referenced there.

## Acceptance
Unit test for the enumeration wrapper with a fake source, GPUI tests for the picker (search, monospace filter, select writes the setting, unknown family warning); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FONT-PICKER)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fontpicker/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
