# SPIKE-AGENT-TELEMETRY — how any agent can hand Hutan its stats: OpenTelemetry and a public report contract

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SPIKE-AGENT-TELEMETRY`

**Contract ownership:** none — a spike: report `docs/spikes/2026-11-03-agent-telemetry.md` + a prototype outside the repo; an ADR draft if a daemon listener is recommended (it is a new attack surface).

## Context
User 2026-11-03: "how will the others give their stats — let's do it for the popular ones". Hutan today gets terminal agents' stats from its OMP extension, Claude/Codex hooks and (INSIGHT-C, in progress) their transcripts. Many agents export **OpenTelemetry** (Claude Code `CLAUDE_CODE_ENABLE_TELEMETRY` + OTLP; Codex `[otel]`; Gemini CLI telemetry; possibly OpenCode, Cursor CLI, Amp, Aider, Goose, Copilot CLI — verify each). A local OTLP receiver would be one generic channel.

## Questions
1. Per popular agent (Claude Code, Codex, Gemini CLI, OpenCode, Cursor CLI, Copilot CLI, Amp, Aider, Goose, Qwen Code — whichever are real): does it export OTel; which signals (metrics/logs/traces), which attributes (tokens in/out/cache, cost, model, tool name/duration/success, session id, file edits, context), protocol (OTLP gRPC / http-protobuf / http-json), how it's enabled (env vars, config file), whether enabling it also sends anything elsewhere, and whether an env var set by the terminal (Hutan sets env for its PTYs) is enough. Also: local transcript/session files each keeps (format, path) and any status/hook API. Primary sources, dated, linked; unverified marked.
2. Prototype outside the repo (`~/hutan-scratch/agent-telemetry/`): a minimal OTLP/HTTP receiver; if any of these agents can be installed in a scratch HOME **without logging in** and emits telemetry on startup or on a failed/unauthenticated call, record the payloads (no model calls, no accounts). Otherwise build payloads from the docs and mark them unobserved.
3. Design: a daemon OTLP receiver (loopback only, per-session token in the endpoint or resource attribute so a process can only report as its own session, bounded, off unless the user opts in per agent), mapping to `AgentInsight`; alternatives (transcripts only; `hutan agent report --insight-json` as the public contract; OSC 7501 for state). Security: who can post, spoofing another session, resource limits, nothing leaves the machine.
4. A public "Report your agent's stats to Hutan" contract for third-party agents: OSC 7501 (state), `hutan agent report` JSON (insight), OTLP (if recommended) — what each carries.
5. Recommendation + slice plan; ADR draft `docs/adr/00NN-agent-telemetry-receiver.md` (status: proposed) if a receiver is recommended.

## Rules
Never read the real `~/.claude`, `~/.codex`, `~/.omp`, `~/.gemini` contents (scratch `HOME` only); no model calls; installs only into scratch dirs (npm/pip/brew prefix inside `~/hutan-scratch/agent-telemetry/`), removed afterwards; never touch the user's Hutan; never pkill by name. Commit the report (and ADR draft) on the slice branch; no `just check` needed for docs only.
