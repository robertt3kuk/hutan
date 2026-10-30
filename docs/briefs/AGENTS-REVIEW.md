# AGENTS-REVIEW — review an agent's turn

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENTS-REVIEW`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.66 if AGENTS-ACTIVITY takes 0.65), `agents.review` (Read), capability `agents.review`, attachment kind `turn_review` (capability per the attachments convention); own commit, goldens.

## Context
User 2026-10-30: Mawas should check what agents did and analyse it. ADR 0025, spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2.5 (read the whole paragraph: the packet's exact contents and the labelled heuristic **signals**), §4.1 row 7, §4.3 R7, R8, R10. Merged: AGENTS-BOARD-B (board, untrusted-data marking for MCP read tools), ASK-SUGGEST.

## Items (in commit order)
1. Protocol `agents.review {session_id, turn_id?}` (default: last finished turn) → the §2.5 packet, ≤ 24 KiB, redacted like an attachment; attachment kind `turn_review`.
2. Daemon builder from the journal's tool events, checkpoints and the diagnostics call; signals as heuristics with names; "not recorded" where the source lacks data (terminal agents: say what is and isn't available); MCP `agent_review` (untrusted-data marking); CLI `hutan agents --review <session> [--turn …] [--json]`.
3. Desktop: *Review this turn* on a finished turn (chat footer, agent row ⋯ / board row) attaches a `turn_review` to the Mawas composer (does not send); the attachment chip opens a readable view of the packet.
4. Docs `docs/assistant.md`, `docs/protocol.md`, `docs/control.md`.

## Acceptance
Daemon tests with fixture journals (commands with exit codes, files, failed tools, each signal, bounds, redaction, terminal agent coverage), MCP/CLI tests, GPUI tests (Review this turn attaches without sending; chip view); goldens; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENTS-REVIEW)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentsreview/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
