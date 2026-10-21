# UX-PROTO-1 — protocol lane: mute and clearing notifications, chat retry, staged/unstaged, real harness mode

Agent: implementer (Claude Sonnet 5.5)   Depends on: E-3 merged (protocol 0.33)   Branch: `slice/UX-PROTO-1`

**Contract ownership:** this brief delegates the additive `core`/`protocol` changes below to you. One minor bump **0.33 → 0.34**, one capability per card, golden fixtures regenerated (`just update-golden`) and reviewed, every new request added to the exhaustive classification table (`crates/daemon/src/router.rs`, `docs/control.md`).

## Context
Read `AGENTS.md`, ADR 0002, `docs/protocol.md`, `docs/control.md`, `docs/design.md`, `docs/process.md` (§ Implementer agents → Resources, § Definition of Done, § UI checks). Cards of `docs/spikes/2026-10-21-ux-plan.md` §6, in this order, one commit each (protocol + daemon + client + desktop + docs per card):
1. **#8 `UX-NOTIFY-CONTROL`** — capability `notify.mute`: per-session mute (`hutan notify mute|unmute <session>`, sidebar/Agents context menu), `notify.sound = "default" | "none"`, no banner/sound for a focused source, banners withdrawn when the session is read or the approval answered elsewhere, desktop acks surfaced intents.
2. **`UX-RETRY` follow-up** — capability `chat.retry`: a `chat.retry {session_id}` (or a `retry` flag on `chat.send`) that resends the last user turn **without journaling the user message again**; the desktop's *Retry now* (merged in UX-ATTN-1, see `docs/gateway.md` retry row) switches to it when the capability is present. Acceptance: one user message in the journal after two attempts.
3. **#19 `UX-CHANGES-INDEX`** — capability `worktree.index`: `DiffFile` gains `staged`/`unstaged` markers; Changes shows staged / unstaged / both and a *Staged · Unstaged · All* toggle. **Read-only** (user decision D2, 2026-10-21): no stage/unstage/commit actions.
4. **#16 `UX-MODE-CHIP`** — capability `chat.mode`: optional `mode` on `ChatInfo` from what the harness reports (verify in `crates/agents/src/acp` what OMP/Claude Code report; Codex already shows its policy). Chip `Mode: …`, a warning state for approve-everything modes, "Mode unknown" when not reported. **Hutan never changes the mode** (user decision D4): a test pins the harness launch arguments.

## Scope
`crates/protocol`, `crates/core` (shared types only), `crates/daemon` (`notify.rs`, `chat.rs`, `worktrees/review.rs`, `harness.rs` read-only for args, `router.rs`), `crates/agents/src/acp` (mode read), `crates/client`, `crates/cli`, `crates/desktop` (menus, settings Notifications page, Changes, chat header), docs (`docs/protocol.md`, `docs/control.md`, `docs/design.md`, `docs/agents.md`, `docs/orchestration.md`, `docs/gateway.md`).

## Acceptance
Each card's T/S acceptance from its plan card; Cua screenshots per `docs/process.md` § UI checks taken before the final gate (if the driver cannot reach a state, say which and rely on GPUI tests). `just check` green (machine-wide lock). Tests deterministic (wait on state/events).

## Parity
List any moved affordance (expected: the *Retry now* path changes transport only).

## Out of scope
Queue/steer (wave 3), checkpoints, stage/commit actions, changing any harness mode. Do not edit `docs/status.md`, `docs/roadmap.md`.

## Report
Per card: wire shapes, tests, screenshots; commands with results; open questions. Every `hutan`/`hutand`/`hutan-desktop` run with a fresh `HUTAN_HOME`; no `hutand` left.
