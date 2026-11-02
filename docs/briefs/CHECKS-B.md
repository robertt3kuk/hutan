# CHECKS-B — Hutan runs the tests itself: agents.verify

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CHECKS-B`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.71 if CHECKS-A takes 0.70), `agents.verify` (Mutate, gated for agents, target-bound like a message), capability `agents.verify`; storage migration (next free — 26 if CHECKS-A takes 25) for `verify_runs`; own commits, goldens.

## Context
User 2026-11-02: checks per workspace/session; developers' top unmet need is false "tests pass" (`docs/spikes/2026-11-02-market/voice.md` §2.3). Design `docs/spikes/2026-11-02-checks.md` §4 (read whole) and §3 `verify_failed`. CHECKS-A (findings engine, running in parallel as another agent) will consume your `verify_runs` for the `verify_failed` detector — expose a small read function for it and coordinate via `write agent://ChecksA`.

## Items (in commit order)
1. Protocol + storage (`verify_runs`: session, command, cwd, started/ended, exit code, duration, output tail ≤ 8 KiB redacted, requested_by user|mawas).
2. Test-command resolution: `[checks] test_command` per project (where project settings live today — find it), else detection by files (Cargo, package.json + lockfile → npm/pnpm/yarn/bun, pyproject/pytest.ini/setup.cfg → pytest, go.mod, Makefile `test:`); return the resolved command and why.
3. Daemon run: in the session's folder (worktree if any), user's login environment like terminals, timeout (default 10 min, setting), bounded output, cancellable, one run per session at a time, refused while the session's own turn runs in the same checkout unless `force` (user only); events for start/end; Mawas path through her gate (target-bound card shows the exact command and folder).
4. MCP `agents_verify` (gated), CLI `hutan agents --verify SESSION [--command …] [--force]` printing the command, then the exit code and tail.
5. Docs `docs/assistant.md` (§ Checks › Verification), `docs/control.md`, `docs/protocol.md`.

## Acceptance
Daemon tests: detection per ecosystem (fixture dirs), a passing and a failing run with exit codes and tails, timeout kills the process tree, refusal during an active turn, Mawas card shows command + folder and Deny runs nothing, run stored and survives restart; CLI/MCP tests; goldens; one real run on a public scratch repo (`~/hutan-scratch/e2e6/repos/ripgrep` copy, `cargo test` of one small crate via `[checks] test_command`) shown in the report; `just check` green. No desktop UI (CHECKS-C).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(CHECKS-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/checksb/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
