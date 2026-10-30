# MAWAS-DIGEST-A — catch up without a model: Catch-up chip and one digest notification

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-DIGEST-A`

**Contract ownership:** none expected (settings + daemon timer + desktop). If a wire change turns out necessary, report it first.

## Context
User 2026-10-30: Mawas should check agents and what was done last time. ADR 0025 (D4: digests Advisor-only — this slice has **no model at all**), spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2.8 steps 1–2 (step 3, the model digest, is NOT in this slice: it waits for the no-network spike), §2.9 (Orca #15148: never one ping per agent). Merged: `agents.activity` + Recent tab, `agents.stats` + Flow (landing), native notifications (`docs/packaging.md`).

## Items (in commit order)
1. **Catch up** chip in Mawas's empty conversation (and the Home empty state): shows the activity `summary` line since last seen (zero tokens); click opens the rail's Recent tab (opening the rail if hidden). Not shown when nothing happened.
2. **Digest notification** (daemon timer, deterministic): when no desktop window has been focused for ≥ N hours (setting, default 4; 0 = off; default on/off decide by spike: default **off** unless the spike says otherwise — state the choice) and something needs the user (pending approvals/questions, failed turns, finished tasks awaiting review) since the last digest, send **one** native notification with the summary line; click opens Mawas Home with Recent. Never one per agent; at most one per window period; quiet hours respected if Hutan has them. Focus/presence comes from the desktop (whatever existing signal the daemon has — check; if none, the desktop tells the daemon via an existing request/notification channel, or compute desktop-side if the desktop process stays alive while unfocused — pick the boring option and say why).
3. Settings rows (Mawas page), docs `docs/assistant.md` (§ Catching up), `docs/packaging.md` if notifications change.

## Acceptance
Unit/daemon tests for the digest decision (thresholds, nothing-to-say → no notification, one per period, off), GPUI tests (chip appears with activity and opens Recent; absent when empty; setting rows); Cua screenshots dark + light of the chip; one real notification observed on a scratch home (screenshot); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-DIGEST-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasdigesta/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
