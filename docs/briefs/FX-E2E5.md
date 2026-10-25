# FX-E2E5 — fixes from e2e user test 5

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-E2E5`

**Contract ownership:** none expected (no protocol change); if F2 needs wire, stop and report.

## Context
Fixes from the real-window pass over the 2026-10-25 features: `docs/spikes/2026-10-26-e2e-usertest-5.md` (read it fully; screenshots under `~/hutan-scratch/e2e5/shots/`, named per finding).

## Items (in commit order; each with a regression test where the brief says so)
1. **F1** File History: list and detail share the width at narrow centre widths (list shrinks to a min, detail never below a readable min; below that the list stacks above the detail). GPUI test at a narrow window.
2. **F2** `hutan open <path>[:line]` opens a file tab in the running window (find why it only prints the path: CLI → daemon → desktop routing; `docs/design.md` / `hutan help`). CLI/GPUI test.
3. **F3** Changes follows Mawas's scope exactly like Files/Search/Problems do (MAWAS-FILES `SessionsStore::files_root`/`set_mawas_focus`): Mawas · Global with a tracked Home folder → Changes shows the Home checkout; update `docs/design.md`/`docs/assistant.md` wording. GPUI test.
4. **F4** Keyboard focus after clicking the Changes History segment (and any segmented control in Changes): global shortcuts (⌘P, ⌘⇧P) keep working — focus goes to a focusable handle. GPUI test.
5. **F6** Worktree setup waiting for trust: *New Terminal in Worktree…*/agent start in that worktree shows a one-line notice with *Review…*; after *Trust and Run* the Setup terminal is revealed (focused tab). GPUI test.
6. **F7** Rewind's "Put a note for <agent> in the composer" actually puts the note in the composer (or the chip shows); GPUI test.
7. **F8** Mawas shows its model: the Mawas header shows backend · model (from the chat's `chat.model`), and the Global empty state says *Ask Mawas…* without "about this project"; investigate the first-Submit-ignored on OMP's question form (fix if reproducible in a GPUI test with the fixture harness).
8. **F9** active sidebar project row keeps its uncommitted count; Changes tab titles include the repo when two checkouts share a branch name (`Changes · alpha · main`; update the test that asserts the old format — it pins incidental wording).
9. **F10** blame gutter: show at least ~24 chars of message (column width from content up to a cap, `editor.blame_width` not needed) and hover shows full commit (already) — check that hover works.
10. **F11** Changes History: the commit list gets the panel's height; the success banner is dismissed when switching segments.
11. **F12** light theme: the active terminal find match clearly differs from others (use the pack's `.active` token with enough contrast; check both bundled light packs).
12. **F13** plural: "1 file in the first commit" — grep similar `{n} files` strings in the slices of 2026-10-25 and fix plurals.
13. **F14** Instructions: when Claude Code already reads AGENTS.md, the Share row says *Optional — for older Claude Code versions* (secondary button), not a contradicting primary.
14. **F15** memory files: in a tracked Home folder, Changes rows under `.mawas/memory/` carry a muted hint *Mawas's memory files*; document in `docs/assistant.md` § Memory files that a new entry is a new section (a `- bullet` under a section stays part of that entry's text).
15. **F16** Automation page: *Open daemon.toml* button; a note that hooks load at daemon start with *Restart daemon…* if one exists in Settings.
16. **F17** read-only revision tab repaint after tab switch — reproduce in a GPUI test (line numbers/content after switching away and back) and fix.

## Acceptance
All items fixed with the named tests; `just check` green; Cua screenshots (dark + light) of F1, F3, F8, F9, F12 into `~/hutan-scratch/fxe2e5/shots/` (launch per `docs/process.md`: `HUTAN_HOME=<scratch> <bundle>/Contents/MacOS/hutan-desktop &`, never `open`).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-E2E5)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxe2e5/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
