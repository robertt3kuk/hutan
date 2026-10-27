# EX-1 — Files tree polish and bench

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/EX-1`

**Contract ownership:** none (desktop; no protocol).

## Context
Spike `docs/spikes/2026-10-27-file-explorer.md` §1.2, §3.2–3.8 and §4 EX-1. Orchestrator decisions on the spike's §5 questions (2026-10-27, spike picks taken; the user can still overrule): Q1 a fresh layout's right body opens on **Files**; Q2 delete = **system Trash only, always confirmed**; Q3 file operations are **not** MCP tools (CLI `hutan fs …` passes the approval policy); Q4 Open Editors **auto** (≥ 2 file tabs); Q5 Files stays right by default, plus a built-in **Explorer left** preset. FX-CHANGES-VIEW (merged before this slice) made Files reachable (layout migration) and added *Show in Files*.

Implement §4 EX-1 items (1)–(9) as written there (bench B-EX with a generated 100k-file tree and budgets in `docs/perf.md`; incremental splice if the bench says so; per-root state cache + persisted expanded set; natural sort, dotfiles toggle, collapse-all, refresh, header ⋯; folded single-child folders, indent guides; keys; the >5 000 entries row; debounce on `fs.changed`; per-type icons), plus the **Explorer left** preset (Q5) and the fresh-layout default of the right body = Files (Q1).

## Acceptance
GPUI tests for each behaviour; bench numbers recorded; Cua screenshots on `~/hutan-scratch/e2e6/repos/ripgrep` and `book` (dark + light, 1440×900 and 1280×800); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(EX-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/ex1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
