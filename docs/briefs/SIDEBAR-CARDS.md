# SIDEBAR-CARDS — agent cards in the sidebar, Superset-like

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SIDEBAR-CARDS`

**Contract ownership:** optional additive minor for `AgentNode.summary` only if needed (take the next free number at merge; own commit).

## Context
Spike `docs/spikes/2026-10-28-best-of/README.md` §1.5–1.6 and §5.3 row 2; UI-STUDY `docs/spikes/2026-10-28-ui-study/README.md` plan row 3 (data part). User decisions (2026-10-28, spike §6 recommendations taken): uppercase 10 px tracked group labels; news line only when there is news; Phosphor Bold ≤ 14 px / Regular above + our own harness monograms; fork destination remembered, always ask on a fallback; session folders user-only, running terminal agents get a pasted `/add-dir` to confirm. No `+/−` counts by default. STYLE-TOKENS and AGENT-TERM-1 (OSC 7501 state, names, harness icons) are merged.

Implement §5.3 row 2: agent cards (lane with monogram + state badge, **session name first**, news line only when something is going on — needs-you question, failure, current tool, unread reply — age, hover ⑂ fork stub hidden until CHAT-FORK-B and × close); shells stay one line; the shared-checkout warning once on the project/worktree row; uppercase group labels; `panels.sessions.show_diffstat` / `panels.changes.show_diffstat` (off). The first agent card without hooks says what installing hooks adds (model, summary) with a link to *Agent integrations…*.

## Acceptance
GPUI tests (row anatomy per state, news-line rules, one shared-checkout warning, diffstat off/on); Cua screenshots with a real `omp` in a scratch terminal and a fixture chat (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SIDEBAR-CARDS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/sidebarcards/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
