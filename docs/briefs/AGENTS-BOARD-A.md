# AGENTS-BOARD-A — lift the agent reducer into a GPUI-free crate

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENTS-BOARD-A`

**Contract ownership:** none (refactor; no protocol).

## Context
ADR 0025 and spike `docs/spikes/2026-10-30-mawas-supervisor.md` §4.1 row 4: the agent reducer behind the Today rail and the sidebar lives in the desktop crate; the daemon must serve `agents.board` later, so the reducer moves into a shared GPUI-free crate first. This is the riskiest step of the Mawas plan — a pure refactor with the desktop tests as the safety net.

## Items (in commit order)
1. Identify the reducer (agent tree / Today state / attention grouping inputs) and its dependencies; design the crate boundary (a new `crates/agents_view` or inside an existing GPUI-free crate — say which and why; dependency rules in AGENTS.md: no GPUI, points inward).
2. Move it with no behaviour change; the desktop uses the crate; unit tests move with it.
3. Prove equivalence: every existing desktop test passes unchanged (list any test you had to touch and why), plus a few unit tests at the new boundary.
4. Docs `docs/architecture.md` (crate list and dependency graph).

## Acceptance
No behaviour change; `just check` green; the new crate builds without GPUI (`cargo tree -p <crate> | grep gpui` empty).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENTS-BOARD-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentsboarda/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
