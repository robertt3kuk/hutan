# CONTAINER-SPIKE — which container runtime for sandboxed agents — research

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/CONTAINER-SPIKE`

**Contract ownership:** none — research spike, docs only.

## Context
User (2026-10-26): "any container — research what's best". SANDBOX-3 (ADR 0021 decision 7, spike `docs/spikes/2026-10-26-sandbox/README.md` § 3) makes a container a *placement* for terminals and agents. Pick the runtime(s) Hutan should support first on macOS (Apple Silicon), with evidence.

## Deliverable
`docs/spikes/2026-10-26-containers/README.md`:
1. Candidates: Apple `container` (apple/container, macOS 26+), OrbStack, Docker Desktop, Colima (Lima), Podman (podman machine), Lima directly, Rancher Desktop. For each: license/cost (Docker Desktop's commercial terms, OrbStack's), install footprint, macOS version requirements, CLI/API Hutan would drive (Docker Engine API socket compat? `container` CLI/XPC?), startup time cold/warm, bind-mount file I/O performance (virtiofs etc.) on a git worktree (measure `git status` and `cargo check`-like workload if a runtime can be installed **with the user's permission only** — default: do not install anything; use published benchmarks and docs, mark [INFERENCE]), PTY support (`exec -it`), Unix socket publishing into the container (to reach `hutand`), networking controls (no network / allowlist), same-absolute-path mounts, rootless, Apple Silicon images.
2. Harnesses inside: how OMP, Claude Code, Codex run in a Linux container (official devcontainer images: Claude Code's devcontainer with its firewall script, Codex's docs), how login happens inside (ADR 0015: Hutan never sees tokens; per-harness credential volume).
3. Detection: how Hutan detects which runtimes are installed and running, without side effects.
4. Recommendation: first-class runtime(s) and why; a `ContainerRuntime` trait shape (detect, create, exec PTY, exec stdio, mount, network, stop); SANDBOX-3 slice plan; risks.
Cite sources (links); unverified claims [INFERENCE].

## Rules
- Work only in `../hutan-wt/CONTAINER-SPIKE`; do not install software, start VMs or pull images (ask the orchestrator if a measurement truly needs it); never touch the user's own Hutan, `~/.omp`, `~/.claude`, `~/.codex` contents or Keychain; never pkill by name.
- Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
- Finish: commit the docs on `slice/CONTAINER-SPIKE`; report the recommendation in 10 lines.
