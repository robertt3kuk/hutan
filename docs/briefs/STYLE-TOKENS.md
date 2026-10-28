# STYLE-TOKENS — Superset-like style tokens, icon weights, harness monograms

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/STYLE-TOKENS`

**Contract ownership:** none (desktop, settings icon pack).

## Context
Spike `docs/spikes/2026-10-28-best-of/README.md` §1 (style spec §1.5, `mock-icons-weight.svg`, `mock-workbench.svg`) and §5.3 row 1. User decisions (2026-10-28, spike §6 recommendations taken): uppercase 10 px tracked group labels; news line only when there is news; Phosphor Bold ≤ 14 px / Regular above + our own harness monograms; fork destination remembered, always ask on a fallback; session folders user-only, running terminal agents get a pasted `/add-dir` to confirm. No `+/−` counts by default. UI-RHYTHM (spacing scale, `ui::sheet`, settings look) is merged first.

Implement §5.3 row 1: the §1.5 tokens as pack entries; `ui::group_label`, `ui::chip_mono`, `ui::row_lane`, `ui::state_badge`; Phosphor Bold files and the `[small]` table in `icon-theme.toml`; own harness monograms behind `harness.*` icon ids; docs `design.md` (reverse the no-all-caps rule for system group labels and panel sections only) and `themes.md`.

## Acceptance
Unit/GPUI tests for the new pieces and icon-weight selection by size (fallback when a pack lacks `[small]`); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(STYLE-TOKENS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/styletokens/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
