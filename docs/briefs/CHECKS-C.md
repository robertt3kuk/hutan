# CHECKS-C — Checks in the window: rail tab, session findings, Check with Mawas

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHECKS-C`

**Contract ownership:** none (desktop only, on the merged `agents.findings` and `agents.verify`).

## Context
User 2026-11-02: Mawas should check a workspace or a session and find what is wrong. Design `docs/spikes/2026-11-02-checks.md` §5 Desktop (read whole). CHECKS-A (`agents.findings`, dismiss) and CHECKS-B (`agents.verify`) are merged; INSIGHT-B feeds terminal agents' insight.

## Items (in commit order)
1. Rail tab **Checks** (Now · Recent · Checks · Memory; `assistant.rail_tab` accepts `checks`), scoped by the Focus like the other tabs (RAIL-FOCUS), grouped by severity then session; each row: title, session, *exact* / *heuristic* label, evidence on expand, *Open* (at the turn), *Review* (attaches the turn review like today), *Verify* (shows the resolved command first; runs `agents.verify`; progress then exit code), *Dismiss*, *Ask Mawas* (prepared ask in the composer, not sent). Coverage line ("2 terminal agents: tools not reported — install the integration"). Refresh like Recent (on show, on turn end, ≤ every 2 s; hidden rail does nothing). Badge on the tab = open alerts in scope.
2. Context panel of a session: **Findings** section (that session's findings, same row actions) and a *Verify* button with the last run's result.
3. *Check with Mawas* in a workspace's ⋯ (sidebar) and a session's ⋯ (tab menu, sidebar row, Agents page): opens Mawas Home, sets the Focus to that workspace, puts a prepared ask with the findings attached in the composer; the user sends.
4. Docs `docs/assistant.md` § Checks (desktop part), `docs/design.md`.

## Acceptance
GPUI tests (common::click_at, wait on state): Checks tab lists findings from a real daemon with fixture journals, Focus filters it, Dismiss hides and survives reopen, Verify shows command then result (fixture project with a passing and a failing test command), Context panel Findings, Check with Mawas fills the composer without sending and sets the Focus; Cua screenshots dark + light; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHECKS-C)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/checksc/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
