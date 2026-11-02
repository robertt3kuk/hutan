# CHECKS-A — findings: deterministic checks over sessions and workspaces

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHECKS-A`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.70 today), `agents.findings` (Read), `agents.findings.dismiss` (UserOnly), capability `agents.findings`; storage migration (next free — 25 today) for dismissed ids; own commits, goldens.

## Context
User 2026-11-02: Mawas should check a workspace or a session and find what is wrong. Design `docs/spikes/2026-11-02-checks.md` §2, §3, §5 (read whole; it is the acceptance). Reuse, don't copy: lift the review packet's signs and test-file/test-command recognition (`crates/daemon/src/agents_review.rs`) into a shared module the findings engine and the packet both use; read insight (`crates/daemon/src/insight.rs`), stats, activity and the state log the way `agents_stats.rs` does.

## Items (in commit order)
1. Shared recognition module (test files, test-like commands, claims of passing in a final message) + the review packet switched to it (no behaviour change; its tests stay green).
2. Protocol + storage.
3. Daemon findings engine: every detector of §3 except `verify_failed`; `[checks]` thresholds with defaults (daemon settings, documented); stable ids; coverage per session; bounds (≤ 200, 24 KiB), redaction; cache per session by journal sequence.
4. MCP `agents_findings` (read, untrusted-data marking, in the read-only list automatically); CLI `hutan agents --check [--ws W | --session S] [--json]`, `--dismiss ID`; playbook lines (design §5 Mawas) within the playbook budget.
5. Docs `docs/assistant.md` (§ Checks), `docs/protocol.md`, `docs/control.md`.

## Acceptance
Unit tests per detector with fixture events (positive, negative, edge), daemon integration tests with fixture Codex/Claude/OMP journals across two workspaces (overlap, shared checkout, coverage for a terminal agent, dismiss is UserOnly and survives restart, stable ids across reads), CLI/MCP tests, goldens; `just check` green. No desktop UI in this slice (CHECKS-C).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHECKS-A)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/checksa/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
