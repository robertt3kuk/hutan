# AGENTS-FLOW — honest flow statistics across agents

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENTS-FLOW`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.67 if AGENTS-REVIEW takes 0.66), `agents.stats` (Read), capability `agents.stats`; storage migration (next free — 23 today) for `agent_state_log`; own commits, goldens.

## Context
User 2026-10-30: Mawas should analyse the flow across agents. ADR 0025 (D3 taken: `agent_state_log` on by default, 14 days), spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2.7 (read the table: what is exact, what is heuristic, the limits) and §4.1 row 8, §4.3 R8, R10, R11. Merged: AGENTS-ACTIVITY (`agents.activity`, storage `in_window` reads, Recent tab).

## Items (in commit order)
1. Storage: `agent_state_log (session_id, at_ms, source, state, kind, message ≤ 120 chars redacted with the history index's redactor)`, written on `agent.report` / terminal-agent state changes, pruned to 14 days; setting (daemon side, e.g. `[agents] state_log = true`, documented) — off stops writing and deletes on request.
2. Protocol `agents.stats {since_ms, until_ms?, group_by: project|harness|day|session|task}` → groups with time per turn/task (wall, active where reported), waits on the user (count, total, longest, by kind; terminal agents from the state log), retries (labelled inferred), tokens per group (exact) and cost (estimate, absent where unknown), recurring failures (normalised, counted; clustering labelled heuristic), stuck/idle flags (*unconfirmed* for terminal agents) — every group carries `coverage`; bounded ≤ 24 KiB.
3. Daemon builder; MCP `agents_stats` (untrusted-data marking); CLI `hutan agents --stats [--since 7d] [--by project] [--json]`.
4. Desktop: a compact **Flow** section in the rail's Recent tab (waits on you, longest wait, failures, tokens by project for the window) with *Analyse with Mawas* (prepared ask in the composer, not sent); Settings row for the state log (Privacy or Mawas page — where SETTINGS-IA puts privacy toggles).
5. Docs `docs/assistant.md`, `docs/protocol.md`, `docs/control.md`, privacy note in `docs/history-index.md` or `docs/assistant.md`.

## Acceptance
Storage tests (write, prune at 14 days, setting off), daemon tests with fixture chats/tasks/terminal agents for each metric and its coverage label and the "not recorded" cases, MCP/CLI tests, GPUI tests (Flow section, Analyse fills composer without sending, setting row); goldens; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENTS-FLOW)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentsflow/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
