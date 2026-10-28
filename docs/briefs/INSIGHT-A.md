# INSIGHT-A — agent insight data: the daemon aggregator

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/INSIGHT-A`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.59 today), `agent.insight` request + `agent.insight_updated` event (only to connections focused on that session), capability `agent.insight`; own commit, goldens.

## Context
Spike `docs/spikes/2026-10-28-insight.md` (phase A: chats via a daemon aggregator; record shape and budgets in its §6/§7) and `docs/spikes/2026-10-28-best-of/README.md` §4 (Context panel). User decisions: a user-initiated *Measure context* (Claude `get_context_usage` / `/context`) rather than polling; transcript tail only for sources enabled in `[history]` (phase C, not now); Codex shows tokens only, no price table.

Implement phase A: a per-session insight aggregate built from what the journal and adapters already deliver (context size/used/window per turn using INSIGHT-FIX's `last` rule, tokens in/out/cache-read/cache-write per turn — add `cache_write` to `TokenCounts` here — cost where the harness reports it (ACP cost, Claude `_meta.quota`), tools used and their counts, files touched from the edit ledger, durations), *Measure context* for Claude Code (breakdown by category) and OMP (fixed overhead vs messages as the spike measured), honest `unsupported` per field; bounded memory; `hutan chat insight SESSION [--json]`, MCP read-only `agent_insight`.

## Acceptance
Daemon tests from recorded fixtures per harness (exact numbers), measure-context with mocks, event only to focused connections, bounds; CLI test; goldens; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(INSIGHT-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/insighta/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
