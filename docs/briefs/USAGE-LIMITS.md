# USAGE-LIMITS — how much you have used and how much is left, per harness and provider

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/USAGE-LIMITS`

**Contract ownership:** **delegated**: additive protocol minor if a new request/event is needed (`usage.summary` / `usage.changed`), capability `usage`; golden fixtures appended.

## Context
Quota and spend visibility is the second-largest gap in the agent CLIs (Claude Code #16157 726, #38335 544; Codex #28879 561, #34035 279; Cursor #126446 479 — `docs/spikes/2026-10-25-user-needs/agents-cli.md`, `editors.md`). Hutan shows per-session context and cost but no account-level picture. ADR 0015: Hutan never reads, stores or forwards harness tokens — everything must come from what the harness itself reports or from local transcripts the user already allowed (history index consent).

## Items (in commit order)
1. **Discovery note** (in `docs/agents.md` § Usage, with sources): what each harness reports — Codex app-server rate-limit notifications/requests (today dropped as "rate-limit chatter", `docs/agents.md` l.42: keep them instead), Claude Code (ACP `usage_update` / cost fields, statusline JSON, transcript token counts), OMP (ACP usage, `omp` commands that print usage — read-only), direct providers (response usage). What is reliable, what is an estimate.
2. **Daemon aggregation**: per harness/account and per provider: tokens and cost today / this week (from turns Hutan ran — exact), plus the harness's own limit windows when it reports them (Codex primary/secondary windows with reset times); stored in SQLite, never secrets; `usage.summary` and a `usage.changed` event.
3. **Desktop**: a status item `usage` (off by default in the built-in bar, on in Settings → Agents: shows the tightest limit: `Codex 72% · resets 3h`), a **Usage** section on the Agents page (per harness: limit bars with reset times when known, tokens/cost today and this week, "estimate" labels where it is one), and a warning toast at 80/95% of a known limit (once per window, respects notification settings).
4. CLI `hutan usage [--json]`; MCP `usage_summary` (read-only).
5. Docs: `docs/agents.md` § Usage, `docs/design.md`, `docs/protocol.md`.

## Acceptance
Daemon tests with recorded Codex rate-limit notifications (fixture) → windows and resets; turn usage aggregation per day/week; nothing stored that looks like a token (redaction test); GPUI tests: status item and Agents page section from a fixture summary; 80/95% toast once per window; CLI test.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(USAGE-LIMITS)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/usagelimits/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
