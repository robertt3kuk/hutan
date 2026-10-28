# FX-DOCK-TOGGLES — right dock collapses like the left; one toggle per dock

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-DOCK-TOGGLES`

**Contract ownership:** none (desktop).

## Context
User 2026-10-28: "the left side closes all the way and the right doesn't; and on the right there are two open/close icons — fix it". The user's screenshots are in `~/hutan-scratch/private/` (`hutan-omp-terminal.png`: Hutan 0.3.1 with OMP running in a terminal tab; `orca-omp.png`: Orca with OMP sessions; `quit-dialog.png`: Hutan's quit sheet). They show the user's **private work** — look at them, never copy their text, names or paths anywhere, never commit them (AGENTS.md invariant). (`hutan-omp-terminal.png` shows the right dock collapsed to a rail of icons while the left one can be hidden completely, a toggle at the top-right of the title bar **and** another at the bottom of the right rail.)

## Items (in commit order; each with a regression test)
1. Reproduce on master (scratch home, Cua) and describe the current states of each dock (expanded / rail / hidden) and every control that toggles them.
2. Symmetry: the right dock can be hidden completely (no rail) the same way as the left — the title-bar toggle cycles/toggles exactly like the left one; the rail-only state stays available as a setting (`layout.right_rail = auto|always|never`, default the behaviour that makes left and right consistent — say which) and for narrow windows (rail peek, EX-2).
3. One toggle per dock: remove the duplicate right-dock toggle (keep the title-bar one, symmetric with the left; keep ⌘L / View menu); update the help/tooltips.
4. Docs: `docs/design.md` § Docks.

## Acceptance
GPUI tests: hide/show both docks to fully hidden, single toggle exists, rail peek still works in a narrow window, layout persistence; Cua screenshots before/after (dark + light) into `~/hutan-scratch/fxdock/shots/` (scratch data only); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-DOCK-TOGGLES)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxdocktoggles/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
