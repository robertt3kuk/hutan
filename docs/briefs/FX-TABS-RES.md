# FX-TABS-RES — One session, one tab; Resources panel that does not overlap

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-TABS-RES`

**Contract ownership:** none (stop and report if a wire change seems needed).

## Context
Two bugs from the user's screenshots (2026-10-22):
- **Same session in two tabs**: a Mawas session showed as two tabs, `Mawas · hutan` (robot icon) and `Mawas` (sparkle icon) — the user confirms it is the same session. Rule: one session = one tab per window; opening a session that already has a tab focuses that tab (from sidebar, Agents page, Mawas Home, palette, notifications, `hutan ui run`); the title and icon of a session are the same on every surface (Mawas sessions use the Mawas sparkle; the scope name follows after ` · ` only when the session has a project scope, everywhere). Find why two items exist (likely `ItemRef::Chat` vs a Mawas Home item pointing at the same session) and fix the cause; add a migration that merges duplicate tabs in saved layouts.
- **Resources panel layout** (screenshot: `Hutan daemon34262` — the description runs into the PID; a session id runs across PID/CPU; `codex-code-mode-…` overlaps the PID; the panel title "Resources" appears twice — panel header and inner header; a row starts with `… hutan main 8 procs` truncated oddly): use fixed column widths with truncation and ellipsis per column (name column flexible, PID right-aligned fixed, CPU/RSS right-aligned fixed), tooltips with the full text, session rows show the session title (not the raw id) with the id in the tooltip, one header only, indentation that never pushes text under the next column. Check narrow widths (280 px) and wide.

## Items (in commit order)
1. Root cause + fix for duplicate session tabs, focus-existing rule, consistent title/icon, layout migration; GPUI tests.
2. Resources panel columns and header; GPUI test that no two text runs in a row overlap (compare laid-out bounds) at 280 px and 600 px.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks), `docs/spikes/2026-10-21-clean-aesthetic.md`, `docs/design.md`. Commit after every item (messages end `(FX-TABS-RES)`); update existing tests to the new spec keeping what they prove; Cua screenshots dark + light before/after into `~/hutan-scratch/fxtabsres/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
