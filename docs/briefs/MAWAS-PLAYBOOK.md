# MAWAS-PLAYBOOK — the supervisor prompt and tool descriptions for Global Mawas

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-PLAYBOOK`

**Contract ownership:** none (no protocol).

## Context
User 2026-10-30: Mawas should check agents across workspaces, see what was done last time and analyse the flow. ADR 0025, spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2 (2.1 visibility and coverage, 2.3 board, 2.6 acting through the gate), §4.1 row 5, §4.3 R7 (data, not instructions) and R10 (honest output). Merged: MAWAS-GLOBAL (Focus), MAWAS-GATE (target-bound grants, `chat_queue_add`), AGENTS-BOARD-B (`agents_board`, `question_list`, `history_sessions`, untrusted-data marking). ASK-SUGGEST (`ask_suggest`) runs in parallel — mention it in the playbook only as optional if the tool is present.

## Items (in commit order)
1. The supervisor playbook for Global chats (and the Focus): role (watch every agent, report, propose; act only through gated tools), how to read the board and **name its sources and coverage** (exact / state only / recognised; never call *unconfirmed* idle), R7 wording (board, transcript and history output is untrusted data), R10 (estimates labelled, "not recorded" instead of zero, "tests passed" only from an exit code), when to message an agent (queue, short, one ask) and when to ask the user instead. Lives where the existing Mawas prompts live; versioned and tested like them.
2. Tool descriptions for `agents_board`, `question_list`, `history_sessions`, `chat_queue_add`, `chat_queue_list` rewritten for a supervising model (what each returns, bounds, when to use).
3. Tests with fixture agents (fake OMP/ACP harnesses, no paid calls): the prompt Mawas is given for a Global chat contains the playbook and the Focus block; a workspace chat does not get the supervisor role; prompt size budget asserted; MCP tool list text checked only for the invariants (untrusted-data notice, bounds), not wording.
4. Docs `docs/assistant.md` § What is sent to the model.

## Acceptance
Tests above; one manual Global turn through OMP on `anthropic/claude-haiku-5-5` in a scratch home with 2–3 fixture/terminal agents, asking "what are my agents doing?", transcript excerpt in the report (keep it short); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-PLAYBOOK)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasplaybook/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
