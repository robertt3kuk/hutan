# REMOTE-SPIKE — remote and mobile control of Hutan — design

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/REMOTE-SPIKE`

**Contract ownership:** none — design spike, docs only.

## Context
Top-4 need (Zed #11473 930, Codex #10450 878, #9224 544, Warp #6831/#6744, Superset #1513, Orca #3099; `docs/spikes/2026-10-25-user-needs/README.md` row 4). Hutan's daemon/client split exists; the protocol is a Unix socket (`docs/protocol.md`); roadmap M9 (remote) waits for an ADR.

## Deliverable (docs only, no product code)
`docs/spikes/2026-10-26-remote/README.md` + a draft `docs/adr/00NN-remote-access.md` (status Proposed) answering:
1. Use cases ranked: watch/approve agents from a phone; drive a daemon on another Mac/Linux box/devcontainer from this desktop; share a session read-only.
2. Transport options compared with evidence (prototype outside the repo under `~/hutan-scratch/remote-spike/` where cheap): SSH-forwarded Unix socket (`ssh -L`/`-R` stream-local), a TLS/WebSocket listener with device pairing, Tailscale-only listener, relay service. Latency on the existing protocol (measure round-trip of `hello`/`session.list`/terminal output through an SSH forward to localhost), auth model (device keys in Keychain, pairing QR), what's exposed (read-only vs approve vs full control — map to the `[control]` policy and ADR 0008/0015 secret rules), backpressure over WAN with bounded queues.
3. Mobile surface: a minimal web client served by the daemon vs native iOS; what subset (attention list, approvals, chat view, stop) and how notifications get to the phone (APNs needs a server; ntfy/Pushover via USER-HOOKS as stopgap).
4. Linux daemon feasibility: which crates are macOS-only (`desktop` only? PTY, Keychain → secret-service), effort estimate per crate.
5. Recommended path with slices (titles, order, protocol impact) and the risks.
Sources cited (links); claims you did not verify marked [INFERENCE].

## Rules
- Work only in `../hutan-wt/REMOTE-SPIKE`; prototypes outside the repo under `~/hutan-scratch/`; never touch the user's own Hutan (`/Applications/Hutan.app`, its data), `~/.omp`, `~/.claude`, `~/.codex` contents or Keychain; no paid model calls (a real harness turn only via OMP on `anthropic/claude-haiku-5-5` if indispensable); never pkill by name.
- Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
- Finish: commit the docs on `slice/REMOTE-SPIKE` (docs only; no `just check` needed, confirm no code changed), report the recommendation in 10 lines.
