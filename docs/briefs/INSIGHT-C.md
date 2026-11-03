# INSIGHT-C — full insight for Claude Code and Codex in a terminal, from their own transcripts

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/INSIGHT-C`

**Contract ownership:** none expected (the insight record and `agent.report.insight` exist since 0.71); report first if a wire change is needed.

## Context
User 2026-11-03: "how will the others give their stats — let's do it for the popular ones, Claude Code and Codex too". INSIGHT-B (merged) gave terminal agents insight through integrations, but Claude Code's and Codex's hooks carry no tokens, context or cost (Codex: no tool times either); the Claude status-line tee is opt-in and gives context and cost only. Their **transcripts** carry it all. Spike `docs/spikes/2026-10-28-insight.md` §3.4, §3.6, §6 row C (read whole; decision 2 there: only for sources the user enabled in `[history]`), `docs/history-index.md` (the `claude` and `codex` adapters already parse these files — reuse them, don't write a second parser).

## Items (in commit order)
1. Find the transcript of a terminal agent session: the hook's `transcript_path` (Claude), the session id the hooks report → the history catalog (Codex `rollout-…jsonl`), else the newest transcript whose cwd matches and that was written after the agent started — say how sure each route is, and never attach a transcript to the wrong session (a test with two agents in one folder).
2. Tail it (only while the session lives and someone focuses it or a detector reads it; bounded reads from the last offset; file rotation/new session handled) and feed the insight reducer per terminal session: Claude assistant `usage` (input, output, cache read/write) per message, the model, tool_use/tool_result with times, files from edit/write tools, `cost` where present, context from `system` context-usage records; Codex `token_count` (last + total, context window), `exec_command_end` (command, exit code, duration), `patch_apply_end` (files), the model. Sources labelled (`transcript`), merged with hooks/status-line by the existing "best source wins" rule, no double counting.
3. Only when `[history] claude`/`codex` is on (the user's consent to Hutan reading those files); the Context panel says "Turn on Claude Code history to see tokens and context" with a one-click enable (the History panel's existing consent flow) when it's off.
4. Detectors (CHECKS-A) automatically gain coverage for these sessions — update the coverage words; `hutan integration status` lists the transcript route.
5. Docs `docs/agents.md` § Insight, `docs/integrations.md`, `docs/history-index.md` (privacy: what is read, when).

## Acceptance
Fixture transcripts (the history index already has recorded Claude and Codex fixtures — reuse and extend; any new shape marked unobserved if not recorded); daemon tests: a terminal Claude Code and a Codex session get tokens, context, cost (Claude), tools with times, files, exit codes; consent off → nothing read; two agents in one folder → no cross-attachment; a transcript rotated mid-session; findings coverage now says the detectors ran. `just check` green. Never read the real `~/.claude`, `~/.codex`, `~/.omp` — scratch `HOME` only.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(INSIGHT-C)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/insightc/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
