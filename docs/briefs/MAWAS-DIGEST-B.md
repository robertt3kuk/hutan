# MAWAS-DIGEST-B — a scheduled model digest, read-only and offline

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-DIGEST-B`

**Contract ownership:** settings + a schedule table (storage migration, next free — 24 today); a protocol minor only if the desktop needs a new request (next free — 0.69); ADR 0017 amendment for unattended Advisor runs (spike `docs/spikes/2026-10-30-mawas-supervisor.md` §4.1 row 10). Own commits.

## Context
ADR 0025 D4 (digests Advisor-only), spike §2.8 step 3, and **`docs/spikes/2026-10-31-advisor-network.md` (read whole — its "What MAWAS-DIGEST-B must enforce" list is the acceptance)**. MAWAS-DIGEST-A gave the deterministic digest (`notify.digest_hours`, `ClientFocus.window_active`, `IntentKind::Digest`, Recent). MAWAS-NET-GUARD gave the web-off overlay for Mawas.

## Items (in commit order)
1. ADR 0017 amendment (and ADR 0025 note): unattended model turns are allowed only for the digest, only on OMP, only with the spike's recipe and the pre-flight check; Claude Code and Codex refused until proven.
2. Daemon: a schedule (`notify.digest_model = off | daily HH:MM | every N h`, default off; the setting shows the expected token cost per run from recent usage) starts a turn in a **separate digest chat** (Global scope, not the user's Mawas chat) with the recipe launch (`--no-tools --no-extensions --no-skills --no-rules --no-context-files --no-lsp`, overlay fetch/web_search off, `mcp.enableProjectConfig false`, only Hutan's MCP server limited to **read** tools), the playbook, and `agents.activity` + `agents.stats` for the window attached. **Pre-flight before the first model call of every launch**: `/tools` all `mcp__hutan_*` and `/mcp list` empty or `hutan` alone (no tokens) — otherwise skip and tell the user why (a notice + the digest chat says so). Pin the tested OMP version: a different version skips with "re-verify" unless the user opts in. Nothing that needs an approval can run (read tools only).
3. Result: the answer is a message in the digest chat plus one notification (reusing `IntentKind::Digest`); click opens the digest chat. A failed/stalled run is reported, never retried in a loop; one run at a time; bounded duration (e.g. 5 min) then cancelled.
4. Desktop: Settings → Mawas → Catching up row for the schedule with the harness requirement and cost; the digest chat appears in *Chats ▾* labelled *Digest*.
5. Docs `docs/assistant.md` § Catching up, `docs/control.md` § Mawas (unattended runs).

## Acceptance
Daemon tests with a fixture OMP (pre-flight pass → turn runs with exactly the recipe args; pre-flight fail on an extra tool / extra MCP server / version mismatch → skipped with the reason; claude/codex refused; schedule fires once per period; stalled run cancelled), GPUI test for the setting row and the Digest chat label; **one real scheduled run** on `anthropic/claude-haiku-5-5` in a scratch home with a 127.0.0.1 canary and a hostile fixture transcript asking to fetch it → digest produced, canary silent; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-DIGEST-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasdigestb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
