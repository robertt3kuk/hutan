# STATUS-METERS — usage in the status bar, in our own format

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/STATUS-METERS`

**Contract ownership:** none (desktop; `usage.summary` exists).

## Context
User 2026-10-28: show usage, but "don't copy Orca's counters — another format is fine". UI-STUDY plan row 6 (`docs/spikes/2026-10-28-ui-study/README.md`), USAGE-LIMITS data (`usage.summary`, per-account windows). Decision: on by default once a harness has reported a limit; hidden otherwise.

## Items (in commit order)
1. Design our own compact form (not Orca's `58% 5h · 41% wk` row): e.g. one small ring/arc per harness showing the tightest window, the harness monogram inside, amber ≥ 80 %, red ≥ 95 %; hover card lists every window (5h, week, per account) with reset times; click opens the Agents page Usage section. Present 2 options as SVG mocks in the report and implement the cleaner one.
2. Status bar item `usage.meters` (replaces the old text `usage` item in the built-in bar when limits exist; the text item stays available), live from `usage.changed`.
3. Settings → Status bar row; docs `docs/design.md`.

## Acceptance
GPUI tests (hidden without limits, thresholds, hover card content, click target, per-account tightest window); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(STATUS-METERS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/statusmeters/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
