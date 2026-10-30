# MAWAS-GATE — target-bound grants for Mawas acting on other agents

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-GATE`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.62 if MAWAS-GLOBAL takes 0.61), `ControlGrant.target_session_id`, `origin` on the target chat's user message, capability `control.grants.target`; storage migration (grants column); own commit, goldens.

## Context
User 2026-10-30: Mawas as the global agent over agents. ADR 0025 (accepted), spike `docs/spikes/2026-10-30-mawas-supervisor.md` §4.1 row 2 and the safety rules §4.3 R1–R8 (read them fully; they are the acceptance). Decision D2 taken: a message Mawas sends to another agent asks **every time, per target agent** — no workspace-wide grant.

## Items (in commit order)
1. Protocol + storage (grants table column for the target).
2. Daemon: target-bound grants for `chat.send`, `chat.send_now`, `chat.queue.add`, `agent.interrupt`, `task.review` (card options *Allow once* / *Allow for this chat and this agent* / *Deny*), the full message text on the card (≤ 2 KiB, longer refused), messages go through the queue (never the user's draft) and carry a provenance label in the target chat, R5 (never into a blocked agent), R6 (no Mawas→Mawas), R8 bounds and the per-turn change cap.
3. MCP `chat_queue_add`, `chat_queue_list` mapped one-to-one onto daemon requests (`readOnlyHint` checked by the existing table test).
4. Desktop: the approval card shows the target agent and full text; the target chat shows *from Mawas* on the delivered message.
5. Docs `docs/control.md`, `docs/assistant.md`, `docs/protocol.md`.

## Acceptance
Daemon tests in `crates/daemon/tests/it/mawas.rs` for each of R1–R8 that this slice touches; GPUI tests for the card and the provenance label; goldens; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-GATE)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasgate/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
