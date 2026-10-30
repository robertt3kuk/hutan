# ASK-SUGGEST — Mawas suggests an answer; you click it

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/ASK-SUGGEST`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.64 if AGENTS-BOARD-B takes 0.63), `ask.suggest {target, answer, reason}` (class Mutate, grantable, advisory only), a `suggestion` field on approval and question DTOs, capability `ask.suggest`; own commit, goldens.

## Context
User 2026-10-30: Mawas as the agent over agents. ADR 0025, spike `docs/spikes/2026-10-30-mawas-supervisor.md` line ~153 (Answer an approval or a question: UserOnly stays; `ask.suggest` attaches an advisory, labelled, unsent suggestion; the user's click on *Use* is the user's own `approval.respond` / `question.answer`; nothing is applied by Mawas — Orca #21745 is the failure this avoids) and §4.3 R2. MAWAS-GATE (merged) gave target-bound grants.

## Items (in commit order)
1. Protocol + daemon: `ask.suggest` stores at most one suggestion per pending approval/question (from a Mawas origin only; the latest replaces the previous; cleared when answered), bounded text; MCP `ask_suggest`.
2. Desktop: approval and question cards show *Mawas suggests: <answer> — <reason>* with **Use** (fills/answers exactly as if the user clicked that option; for a free-text question, fills the field without sending) and **Dismiss**; never auto-applied; the suggestion is visually distinct.
3. CLI `hutan approvals` / `hutan questions` show the suggestion.
4. Docs `docs/assistant.md`, `docs/control.md`, `docs/protocol.md`.

## Acceptance
Daemon tests (suggest on approval and question, replace, clear on answer, non-Mawas origin refused, UserOnly answer unchanged), GPUI tests (Use answers via the user's own path, Dismiss, free-text fill without send), CLI test, goldens; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(ASK-SUGGEST)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/asksuggest/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
