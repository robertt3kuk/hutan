# SANDBOX-2 — Contain: Seatbelt boundary for terminals and agents without a native sandbox

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SANDBOX-2`

**Contract ownership:** **delegated**: additive protocol minor, capability `sandbox` (`contain?` on `terminal.create`/`chat.create`/task runs, `SessionInfo.containment {kind: seatbelt, network, write_roots, applied, reason?}`, `daemon.toml [sandbox]`) per ADR 0021 decision 8; own commit; protocol **0.52**.

## Context
ADR 0021 (Accepted) and `docs/spikes/2026-10-26-sandbox/README.md` § 2 (prototype profiles in the appendix, measurements, the no-nesting rule) and § 4 SANDBOX-2 define this slice. SANDBOX-1 left OMP's *Contained* refused "needs Contain (SANDBOX-2)"; this slice provides it.

## Items (in commit order)
1. Protocol `sandbox` (0.52).
2. Daemon: a `Contain` module that wraps a spawn (PTY or stdio) in `sandbox-exec -p <profile>` generated from a typed policy (write roots = the checkout + its git common dir + temp dirs the spike lists; network on/off; deny Keychain); a startup self-test (spawn a contained `/usr/bin/true` and a write probe) — failing closed with a reason when Seatbelt is unavailable; never nests: a session whose harness uses its own OS sandbox (Codex/Claude under SANDBOX-1 *Contained*) is refused with a reason.
3. Wire into terminals (`terminal.create.contain`), terminal agents and OMP chats (*Contained* for OMP now maps to Contain + `--approval-mode` per ADR); `SessionInfo.containment` read from what was actually applied.
4. Desktop: Start sheet *Contained* for OMP and terminals enabled; session chip with honest wording (*writes limited to this worktree · can read your files · network off/on*); `[sandbox]` settings.
5. CLI `hutan new --contain [--network off]`, `hutan chat new --safety contained` for OMP.
6. Docs: `docs/agents.md` § Safety presets, `docs/design.md`, `docs/architecture.md`.
(SANDBOX-2B, the per-session restricted daemon socket, is a later slice — out of scope.)

## Acceptance
Daemon tests on this Mac (Seatbelt is available): writes outside the roots fail with EPERM, inside succeed, git in a linked worktree works, network off blocks a loopback TCP connect, self-test failure refuses; nested-sandbox refusal; GPUI tests for the enabled presets and chip; CLI tests; golden fixtures; Cua screenshots of a contained terminal showing the EPERM and the chip (dark + light).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SANDBOX-2)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/sandbox2/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
