# INSIGHT-B — terminal agents' insight: tools, files and tokens from the integrations

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/INSIGHT-B`

**Contract ownership:** per `docs/spikes/2026-10-28-insight.md` §6 row B (the reporter `insight` field exists since 0.58/0.59 — confirm; if a wire change is needed, take the next free minor at merge and say so first).

## Context
User 2026-11-02: checks per session must cover agents running in terminals too; today Hutan knows only their state. Spike `docs/spikes/2026-10-28-insight.md` §3.2, §3.4, §3.6, §5 and §6 row B (read whole; row B is the scope), design `docs/spikes/2026-11-02-checks.md` §6. INSIGHT-A (merged) has the reducer and aggregator for chats.

## Items (in commit order)
1. OMP extension (installed by `hutan integration install omp`): `insight` reports at `agent_end` and at most every 500 ms during a turn — usage of the call, cost, tools with times, files from tool args, context and the fixed/messages split at session start/switch/compaction/model change.
2. Claude Code hook script: tools, files, `duration_ms` (PostToolUse); the **opt-in** status-line tee (`hutan integration install claude --statusline`) recording the JSON for Hutan and chaining to the user's own command (never overwrite it; uninstall restores it).
3. Codex hooks for tools and files, as far as Codex's hooks allow (say what they don't).
4. Daemon: those reports feed the same insight reducer per terminal session; the Context panel and `hutan chat insight` show them with source labels; `hutan integration status` says what each installed integration reports.
5. Docs `docs/agents.md` § Insight, `docs/integrations.md`.

## Acceptance
Fixtures recorded from scratch runs (OMP extension events; Claude status-line JSON only if you can produce it without the user's account — otherwise build from the documented shape and mark unobserved); daemon tests feeding them through the reducer; the integration install/uninstall tests (user's own status-line command preserved); real OMP terminal run on `anthropic/claude-haiku-5-5` in a scratch home showing tools/files/tokens in the Context panel (screenshot dark + light); `just check` green. Never read `~/.omp`, `~/.claude`, `~/.codex` contents; use scratch `HOME`/config dirs.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(INSIGHT-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/insightb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
