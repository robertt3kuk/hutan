# AGENTS-BOARD-B — the board of every agent, served by the daemon

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENTS-BOARD-B`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.63 if MAWAS-GATE takes 0.62), `agents.board` (Read), capability `agents.board`; own commit, goldens.

## Context
User 2026-10-30: Mawas as the global agent over agents. ADR 0025, spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2 (board) and §4.1 row 4 (rest of AGENTS-BOARD), safety rules §4.3 (R7 data-not-instructions, R8 bounds ≤ 200 agents / 24 KiB, R10 honest coverage). AGENTS-BOARD-A (merged) lifted the reducer into `hutan-agents-view`.

## Items (in commit order)
1. Protocol `agents.board {workspace_id?}` → every agent across workspaces (state, kind, what it does / waits on, since, harness, model, coverage per row: reported by hooks / OSC 7501 / recognised only), bounded; capability `agents.board`.
2. Daemon builds it with `hutan-agents-view` from its own state (window-only sets empty); MCP `agents_board`, `question_list`, `history_sessions` (read-only, `readOnlyHint` checked by the table test), output marked as untrusted data for the model (R7); CLI `hutan agents --board [--json]`.
3. Mawas rail tabs **Now** (the board, grouped like the Agents page, click opens the agent) and **Memory** (what the rail's Memory link opened), rail width draggable and remembered (`assistant.rail_width_rem`).
4. Docs `docs/assistant.md`, `docs/protocol.md`, `docs/control.md`.

## Acceptance
Daemon tests (board across workspaces with fixture agents, coverage labels, bounds), MCP/CLI tests, GPUI tests for the rail tabs and width; goldens; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENTS-BOARD-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentsboardb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
