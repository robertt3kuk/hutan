# EX-2 — narrow windows: the right body peeks instead of disappearing

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-2`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-27: "it must be possible to fit things to the window size" and "there is no file explorer". The workability report (`docs/spikes/2026-10-27-workability.md` F-05) found the explorer auto-hidden at ≤ 1 000 px; the spike `docs/spikes/2026-10-27-file-explorer.md` §3.7 and §4 EX-2 define the fix: below the width where the center would drop under 480 px, a right-dock panel opens as a **peek** over the center instead of squeezing it or vanishing. Implement §4 EX-2 items (1)–(5) as written there, for every right-dock panel (Files, Changes, Agents, History…), plus the left sidebar's compact mode at ≤ 900 px keeping Files reachable when the user moved Files left (Explorer-left preset from EX-1).

## Acceptance
GPUI tests over the §3.7 decision table (window widths × dock widths → push/peek/rail), peek open/close (Esc, outside click, opening a file, same icon), keyboard focus in and back, setting `layout.rail_peek`; Cua screenshots at 1100 and 900 px, before/after (dark + light) into `~/hutan-scratch/ex2/shots/`; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-2)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex2/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
