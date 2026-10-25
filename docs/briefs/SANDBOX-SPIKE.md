# SANDBOX-SPIKE — sandboxed agent runs — design

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SANDBOX-SPIKE`

**Contract ownership:** none — design spike, docs only.

## Context
Orca #13665, Superset #2419, #3957 ask for agents to run sandboxed (container or OS sandbox) so approvals aren't the only guard (`docs/spikes/2026-10-25-user-needs/README.md` row 14). Hutan relies on harness approvals and its own `[control]` policy; harnesses have their own sandboxes (Codex seatbelt/landlock `sandbox_mode`, Claude Code permission modes and `sandbox`, OMP ?).

## Deliverable (docs only, no product code)
`docs/spikes/2026-10-26-sandbox/README.md` + draft ADR (status Proposed):
1. What each harness already offers (docs + `--help`, cite): modes, how to turn on per session, what Hutan could pass (flags/config) — likely the cheapest win: expose harness sandbox modes in the Start sheet.
2. OS-level options on macOS: `sandbox-exec` profiles (deprecated but used by Codex), App Sandbox (not applicable), Endpoint Security (no); prototype a `sandbox-exec` profile for "write only inside the worktree, network off/on" under `~/hutan-scratch/sandbox-spike/` running a shell and a harness with a dummy task; measure overhead and what breaks (git, package managers, Keychain).
3. Container options: Docker/OrbStack/Apple `container` CLI devcontainers — worktree mounted, harness inside, credentials (ADR 0015: tokens never pass through Hutan — how would the harness log in inside?), PTY bridging, performance on macOS.
4. Recommended path: slices (e.g. SANDBOX-1 harness modes in Start sheet; SANDBOX-2 seatbelt profile for terminals/agents per worktree; SANDBOX-3 devcontainer placement), protocol impact, risks.
Sources cited; unverified claims [INFERENCE].

## Rules
- Work only in `../hutan-wt/SANDBOX-SPIKE`; prototypes outside the repo under `~/hutan-scratch/`; never touch the user's own Hutan (`/Applications/Hutan.app`, its data), `~/.omp`, `~/.claude`, `~/.codex` contents or Keychain; no paid model calls (a real harness turn only via OMP on `anthropic/claude-haiku-5-5` if indispensable); never pkill by name.
- Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
- Finish: commit the docs on `slice/SANDBOX-SPIKE` (docs only; no `just check` needed, confirm no code changed), report the recommendation in 10 lines.
