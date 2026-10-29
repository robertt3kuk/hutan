# FX-UI-A — sidebar, tabs, names and states after the real-window test

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-UI-A`

**Contract ownership:** none (desktop, terminal; no protocol).

## Context
Real-window evaluation `docs/spikes/2026-10-29-ui-wave-test.md` (screenshots `~/hutan-scratch/e2e8/shots/` by number) of the UI wave (user 2026-10-28: a normal UI, nicer cards, session names first). Style spec `docs/spikes/2026-10-28-best-of/README.md` §1.5.

## Items (in commit order; each with a regression test)
1. **P2-1** measuring context never marks a chat unread or adds a news line/tab ✓ (the measurement is not a turn anywhere — reuse `is_measure_message`).
2. **P2-3 / fix 1** forks readable: a fork's default name gets a parent-relative suffix (`⑂ 2` / `⑂ wt branch`) and rows/tabs middle-truncate so parent, fork and worktree-fork never read identical.
3. **P2-6 / fix 4** harness monogram for chats too (OMP chat π, Claude asterisk, Codex chevron) in rows, tabs and the Agents page; the tab glyph carries the state badge like the sidebar.
4. **P2-7 / fix 5** ended / exit-0 rows: hollow ring, keep the agent glyph, stay out of READY FOR REVIEW when nothing changed; **P3-20** a plain shell finishing a command out of sight doesn't get bold + ✓ (only a bell on BEL/notification).
5. **P3-8** strip emoji/pictograph prefixes from agent titles (`➖ SUB:` → `SUB:`), titles in the name weight; **P3-9 / fix 7** the hooks hint becomes a one-time dismissible ⓘ on the first agent row (not a permanent second line).
6. **P3-10 / fix 6** calm project row: one branch chip, one counter; the uncommitted count gets its own glyph (not the working ring), drop or explain `5/7` (tooltip + a clear label).
7. **P3-11 / fix 9** tooltips anchored below-right of the pointer with a short delay, never covering the hovered control; hover icons push the title's ellipsis. **P3-12** tab strip overflow: fade + chevron menu, no lone half tab. **P3-19** Agents page rows use the sidebar card anatomy. **P4** group label alignment (FAILED/REQUESTS vs PROJECTS), the REQUESTS question line starts at the name.
8. Docs `docs/design.md`.

## Acceptance
GPUI tests per item; Cua before/after screenshots (dark + light) with a real `omp` on Haiku in a scratch terminal and fixture chats; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-UI-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxuia/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
