# CONTEXT-PANEL — the Context panel on the right

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CONTEXT-PANEL`

**Contract ownership:** none (desktop; uses 0.59 `agent.insight`).

## Context
User 2026-10-28: "on the right, see how the context is filled where possible, and fork the session from any point". Spike `docs/spikes/2026-10-28-best-of/README.md` §4 and `mock-context.svg`; `docs/spikes/2026-10-28-insight.md`. INSIGHT-A merged: `agent.insight` / `agent.insight_updated` (only to connections focused on the session), `agent.measure_context`, `AgentInsight {context, totals, turns, tools, files, time, sources, unsupported, can_measure}`. CHAT-FORK-B (running, `ChatForkB`) owns the fork popover — call its action from turn rows, don't duplicate it.

## Items (in commit order)
1. A registered right-dock panel **Context** (`panels.context.*`, rail icon, ⌘K, default in the right rail after Files and Changes) following the focused agent session (chat or terminal agent; empty state otherwise).
2. Header: context fill bar (used / window, with the harness's own numbers; *estimated* where marked), *Measure context* button when `can_measure` (shows the breakdown by category: system, tools, memory files, messages… as a stacked bar + list).
3. Totals: tokens (in/out/cache read/cache write), cost when reported, time; tools used (counts); files touched (click opens the file / its diff).
4. **Turn rows** (newest first, virtualised): time, tokens, tools, files changed; each row has *Fork…* (CHAT-FORK-B's popover at that turn) and *Rewind…* (existing checkpoint restore) — the timeline.
5. Honest empty states per harness (`unsupported` fields named, e.g. "Codex reports tokens, not cost").
6. Docs: `docs/design.md`, `docs/agents.md` § Insight.

## Acceptance
GPUI tests over a real daemon with fixture chats (OMP, Codex, Claude shapes): numbers shown match the fixture, measure-context flow with a mock, turn-row Fork opens the popover at that turn, empty states; Cua screenshots (dark + light) in the STYLE-TOKENS look; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CONTEXT-PANEL)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/contextpanel/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
