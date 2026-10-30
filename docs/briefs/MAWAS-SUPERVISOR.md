# MAWAS-SUPERVISOR — Mawas as the global assistant over all agents (design)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-SUPERVISOR`

**Contract ownership:** none — design spike, docs only (draft ADR if a hard-to-reverse decision appears).

## Context
User 2026-10-30 (after updating to 0.3.4 in the app — it worked): "Mawas should also be global as an assistant across all projects… it should be global and connect to the agents; an assistant that can check our agents across the workspace, look at what was done last time and analyse our flow — the main thing is an agent over agents"; "its chat width should be adjustable"; "the Memory part of Mawas — be able to hide the right part, or add something useful there". Today Mawas has scopes (Global / per workspace), modes (Ask before acting / Advisor), memory, Today rail (requests, working, schedules), MCP tools over Hutan (sessions, tasks, history, usage, insight…) — read `docs/assistant.md`, ADR 0017, `docs/control.md` (Mawas's gate), `docs/history-index.md`, `docs/agents.md` § Insight.

## Deliverable
`docs/spikes/2026-10-30-mawas-supervisor.md` (+ SVG mocks of the Mawas page):
1. **Global first**: what changes if Mawas defaults to Global and a workspace becomes a filter/context instead of a separate assistant; migration of existing workspace Mawas chats/memory.
2. **Agent over agents** — concrete capabilities with the data/tools that already exist and what's missing: a live board of every agent across workspaces (state, what it's doing, waiting on whom), "what happened since I was away / last time" (journal + history index + checkpoints + insight), reviewing an agent's turn (diff, tests run, risks), nudging/answering/stopping agents (through Mawas's gate — what needs user approval), flow analysis (time per task, waits on the user, retries, cost/tokens per project, recurring failures) with honest data sources, scheduled check-ins. Compare with how Orca / Superset / Conductor / Claude Code subagents / Codex cloud present a supervisor or "manager" view (cite).
3. **Layout**: resizable chat column (drag, remembered), the right rail: hide/show toggle and what useful content belongs there (agents board, recent changes, memory) — mock it.
4. **Plan**: slices in commit order with protocol impact (new MCP tools, daemon aggregation), safety rules (Mawas never acts without the gate), and a quick-win slice for the layout (chat width + rail toggle) that can start immediately.

## Rules
- Docs only; work in `../hutan-wt/MAWAS-SUPERVISOR`; screens/mocks under `~/hutan-scratch/mawassup/`; never touch the user's Hutan or harness credentials/sessions; don't build the workspace (another agent's debug build may be reused); scratch `HUTAN_HOME` for any look at the app; Cua: input to ABC, restore Russian; the user's screenshots never enter the repo. Don't edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`. Commit on `slice/MAWAS-SUPERVISOR`; report in ≤15 lines.
