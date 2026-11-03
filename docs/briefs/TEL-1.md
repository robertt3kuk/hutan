# TEL-1 — the loopback OTLP receiver (ADR 0026)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/TEL-1`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.74 today), `telemetry.status` request + capability `agent.telemetry`, `source: otlp` in insight; new crate `crates/telemetry` (GPUI-free; dependency rules in AGENTS.md); `prost` into root `[workspace.dependencies]`; own commits, goldens.

## Context
ADR 0026 (accepted 2026-11-03; read it whole — its Decision list is the acceptance and review blocker list) and spike `docs/spikes/2026-11-03-agent-telemetry.md` (§ 3 facts F1–F11, § 4 design and threat model § 4.3, § 6 row TEL-1; observed payloads in `docs/spikes/2026-11-03-agent-telemetry/fixtures/`). User decisions: receiver yes, off by default, per agent; transcripts stay primary for Claude Code/Codex (INSIGHT-C, in progress) and OTLP is the opt-in live upgrade; random persistent port in `daemon.toml`. TEL-0 (docs + schema + conformance, running in parallel) owns `docs/integrations.md` § "Report your agent's stats" — coordinate.

## Items (in commit order)
1. `crates/telemetry`: OTLP decode (protobuf subset via prost, and OTLP-JSON), chunked bodies without Content-Length, allowlist mappers (generic GenAI semantic conventions, Claude Code, Codex logs) → the existing `InsightReport` shape; unknown attributes ignored; content-bearing attributes (prompts, tool args/output, system instructions — spike F-list) never read or stored.
2. Daemon: listener bound to `127.0.0.1` only on `[telemetry] port` (0 → pick once in the ephemeral range, persist; busy → stays down and visible, never rebinds elsewhere), opened when the first agent is enabled and closed with the last (`[telemetry.agents] <agent> = true`, user-only setting); per-session token registry (a process can only report as its own session), request size/rate limits, always answers immediately (mapper work off the request path; the < 50 ms bound with a stalled mapper), bounded queues; reports feed the insight reducer as `source: otlp` under the best-source ranking; `HUTAN_OTLP_ENDPOINT` / `HUTAN_OTLP_TOKEN` exported into a session's PTY only while telemetry is on.
3. Protocol `telemetry.status` (listener state, port, enabled agents, last report per session) + capability; CLI `hutan telemetry status|enable <agent>|disable <agent>`.
4. Docs `docs/integrations.md` (telemetry section, coordinating with TEL-0), `docs/protocol.md`, `docs/architecture.md` (new crate), `docs/control.md` (user-only setting).

## Acceptance
Real-socket daemon tests: off → no socket; enable → bound on loopback only; wrong/missing token rejected; a session can't report as another; size/rate limits; the < 50 ms answer with a stalled mapper; fixture round-trips for Claude Code, Codex and a generic GenAI payload (protobuf and JSON) into the insight record; content attributes never stored; busy persisted port → down and reported. A cargo-fuzz (or proptest) target for the decoder. `just check` green. Not in this slice: enabling agents' own export (TEL-2), other agents' mappers (TEL-3), Settings UI (TEL-4).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(TEL-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/tel1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
