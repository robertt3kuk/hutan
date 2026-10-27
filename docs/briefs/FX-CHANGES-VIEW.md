# FX-CHANGES-VIEW — Changes view scrolling, file list, Files beside Changes

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-CHANGES-VIEW`

**Contract ownership:** none (desktop only; no protocol change).

## Context
User report 2026-10-27 with a screenshot (`~/hutan-scratch/fxchangesview/user-report.png` — the user's private work repository: never commit it, never copy its text): in the Changes view of a real repository (88 new Markdown files in nested folders, +6124 lines, long lines with Cyrillic text and Markdown tables, *With uncommitted* on) "scrolling does not work correctly at all", and "where is the Files view, right beside it, to see what is in this workspace or folder". The user ran the 2026-10-23 build (0.2.0+); reproduce everything on current master first and fix what still fails.

What the screenshot shows:
- The diff pane is scrolled horizontally: every line starts cut off on the left, the line-number/sign gutter is gone, and the added-line green background is a fixed-width band (~230 px) that does not cover the text, so the tint and the text are out of step.
- The file list shows `ZP-3814/hand...`, `ZP-3815/revi...`: the folder prefix eats the width, so files can't be told apart.
- The right rail shows History, Changes, Requests, Agents icons — no Files.

## Items (in commit order; each with a regression test)
1. **Reproduce** on master with a scratch repo shaped like the user's (nested folders, ~90 untracked `.md` files with 200-character Cyrillic lines and Markdown tables, uncommitted): record what fails (vertical wheel/trackpad scroll of the file list and of the diff, horizontal scroll, scrollbars, keyboard scroll) with Cua screenshots before the fix.
2. **Diff scrolling**: vertical scrolling works through the whole diff with wheel, trackpad and keys, independent from the file list; horizontal scrolling keeps the gutter (line numbers, +/−) fixed and moves only the code; added/removed backgrounds span the full scrolled content width; switching files resets the horizontal offset; a horizontal scrollbar shows when content is wider. A **Wrap lines** toggle in the diff header (`changes.wrap_lines`, default **on** for Markdown/text files, off for code — or one global default if per-language is costly; say which) avoids horizontal scrolling for prose.
3. **File list**: each row shows the **file name first** and the folder muted after it (middle-ellipsis on the folder, never on the name), full path in the tooltip; an optional **tree** grouping by folder (`changes.list = flat|tree`, default flat) with collapsible folders and counts.
4. **Files beside Changes**: the Files panel is reachable on the right rail next to Changes by default; find why it can be missing (saved layout from an older version, hidden by the user, preset) — a saved layout that lacks Files without the user having hidden it gets it back on the right (layout migration, one-time note per the parity rule); Changes rows and the diff header get *Show in Files* (reveals the file in the Files panel, scoped to the same checkout); the Files panel follows the checkout the Changes view shows.
5. Docs: `docs/design.md` (§ Changes view), CHANGELOG is the orchestrator's.

## Acceptance
GPUI tests: wheel/trackpad scroll events move the diff and the list independently to the end; horizontal scroll keeps the gutter in place and the tint full width; offset resets on file switch; wrap toggle; name-first rows; tree grouping; Files present after loading an old saved layout without it; Show in Files. Cua screenshots before/after (dark + light) of the scratch repo into `~/hutan-scratch/fxchangesview/shots/`. `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-CHANGES-VIEW)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxchangesview/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
