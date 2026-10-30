# AGENTS-ACTIVITY — what happened since I was away: activity feed and the Recent tab

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENTS-ACTIVITY`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.65 if ASK-SUGGEST takes 0.64), `agents.activity` (Read), capability `agents.activity`; own commit, goldens.

## Context
User 2026-10-30: Mawas should see what was done last time across agents. ADR 0025, spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2.4 ("What happened since I was away"), §4.1 row 6, §4.3 R7, R8 (≤ 200 events, ≤ 24 KiB), R10. AGENTS-BOARD-B (merged) added `agents.board`, the rail tabs Now/Memory and `assistant.rail_tab`.

## Items (in commit order)
1. Protocol `agents.activity {since_ms?, workspace_id?, limit?}` → bounded events across agents (turn finished/failed, approval asked/answered, question asked/answered, task done/review, files changed count, exit codes, session started/ended) built from what the daemon already records (journals, task store, session events) — list what exists and what is "not recorded"; per-client *last seen* mark (`agents.activity` returns it; a Read-classified `seen` update only if the spike says so, otherwise desktop-side).
2. Daemon builder + MCP `agents_activity` (untrusted-data marking like the board); CLI `hutan agents --activity [--since …] [--json]`.
3. Rail tab **Recent**: events since last seen grouped by agent, last-seen line, *Summarise with Mawas* (puts a prepared ask into the Mawas composer — does not send).
4. Docs `docs/assistant.md`, `docs/protocol.md`, `docs/control.md`.

## Acceptance
Daemon tests (events across two workspaces with fixture agents, since filter, bounds, redaction), MCP/CLI tests, GPUI tests (Recent tab, last seen, Summarise fills the composer without sending); goldens; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENTS-ACTIVITY)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentsactivity/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
