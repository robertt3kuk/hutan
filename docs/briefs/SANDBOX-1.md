# SANDBOX-1 — safety presets in the Start sheet (harness-native sandboxes)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SANDBOX-1`

**Contract ownership:** **delegated**: additive protocol minor, capability `chat.safety` (`chat.create.safety?`, `ChatInfo.safety {preset, enforced_by, network?}`, `TaskRun::Chat.safety?`) exactly as ADR 0021 decision 8 sketches; own commit; protocol **0.51**.

## Context
User (2026-10-26) wants agents sandboxed and containers researched ("any container, research what's best"). Spike `docs/spikes/2026-10-26-sandbox/README.md` (§ 1 harness facts, § 4 SANDBOX-1) and draft ADR 0021 (`docs/adr/0021-sandboxed-agent-runs.md`) define this slice; implement § 4 SANDBOX-1 as written there, and set ADR 0021's status to *Accepted (2026-10-26, user asked for sandboxing; SANDBOX-1 first)* with a line noting the open questions still open (contained-autonomy default, containers after CONTAINER-SPIKE).

## Items (in commit order)
1. Protocol `chat.safety` (0.51).
2. Daemon: preset → per-harness arguments (Codex `sandbox`/`approval_policy`/network; Claude Code session mode + `_meta.claudeCode.options` sandbox settings in `session/new`; OMP `--approval-mode`), in the code that builds harness launch profiles; verify Codex's approval vocabulary against the pinned adapter version and the recorded fixtures before using `untrusted` (spike § 1 risk). Effective state read back from what the harness reports.
3. Desktop: Start sheet **Safety** row (*Ask* default · *Contained* · *Read-only* · *Full access* in warning colour), only presets the harness can honour enabled with the reason; chat header chip shows the *effective* state with honest wording (ADR 0021 decision 6); `[launcher] safety` default per project/global.
4. CLI `hutan chat new --safety`, `hutan task add --safety`; MCP read-only exposure in `ChatInfo`.
5. Docs: `docs/agents.md` § Safety presets (per-harness table with sources), `docs/design.md`.

## Acceptance
Daemon tests with fixture harnesses assert the exact arguments per preset and harness and the effective state read back; presets the harness cannot honour are refused with a reason; GPUI tests for the row, disabled presets with reasons, chip; CLI tests; golden fixtures.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SANDBOX-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/sandbox1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
