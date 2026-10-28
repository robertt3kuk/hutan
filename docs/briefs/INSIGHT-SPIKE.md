# INSIGHT-SPIKE — what each harness exposes for an agent insight panel

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/INSIGHT-SPIKE`

**Contract ownership:** none — research spike, docs only.

## Context
User 2026-10-28: "later, add tools so that in the sidebar we can see the focused agent's analytics or context — what costs what". Study `docs/spikes/2026-10-28-ui-study/README.md` §4 and plan row 7 (`AGENT-INSIGHT`, mock `mock-insight.svg`).

## Deliverable
`docs/spikes/2026-10-28-insight.md`: per harness (OMP via ACP and in a terminal, Claude Code via ACP and terminal, Codex app-server and terminal) — what is exposed about the context window (size, used, breakdown by system/tools/files/history), tokens and cost per turn, tools used, files touched, time; through which channel (ACP `usage_update` and `_meta`, Codex `thread/tokenUsage/updated` / rollout `token_count`, Claude Code `get_context_usage` [verify], OMP extension API / hooks / session JSONL / OSC 7501); verified from docs, `--help`, source on GitHub and **recorded fixtures already in this repo**; where real runs are needed use OMP on `anthropic/claude-haiku-5-5` only, in a scratch dir, a few turns. What Hutan already stores (Inspector panel, usage tables, journal). Then the phase A/B/C slice plan with protocol shape (`agent.insight` request + event) and budgets.

## Rules
- Work only in `../hutan-wt/INSIGHT-SPIKE`; no product code; never read harness credentials or the user's own sessions (`~/.omp`, `~/.claude`, `~/.codex` contents are off limits — use a scratch `HOME`/config dir for any run); do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`. Finish: commit on `slice/INSIGHT-SPIKE`, report in ≤12 lines.

