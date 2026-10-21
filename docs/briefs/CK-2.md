# CK-2 — checkpoints per agent turn: wiring, store, retention

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-1 merged   Branch: `slice/CK-2`

**Contract ownership:** delegated: shared types in `crates/core` (`CheckpointId`, `CheckpointInfo`) and a SQLite migration in `crates/storage`. No protocol change in this slice (CK-3 adds the wire).

## Context
Read `AGENTS.md`, ADR 0019 (accepted: default **on** for agent chats in git checkouts; Decision 1–12), `docs/spikes/2026-10-21-checkpoints.md` §4–§7 row **CK-2** (verbatim acceptance), CK-1's report in `docs/status.md` and the engine API `crates/daemon/src/checkpoints/` (`Checkpoints::snapshot(checkout, Label)`; note CK-1 writes `<turn>-start` and `<turn>-end` refs — ADR wants one ref per turn: keep the end ref with the start commit as its parent and delete the start ref once the end is written), `crates/daemon/src/chat.rs` (`ChatManager::send`, the pump's turn end), `crates/storage` migrations, `daemon.settings` (ST-3) for config.

## Scope
`crates/core` (types), `crates/storage` (migration: one row per checkpoint with session, turn, refs, bytes, skipped summary, state), `crates/daemon` (config `[checkpoints] mode = "on"|"off"`, caps and retention via `daemon.settings`, per-project override; start snapshot in `ChatManager::send` with a 5 s budget then `late`, end snapshot at turn end incl. interrupted/failed turns, no-change turns recorded as such without a new ref, pre-warm at chat open, reconcile rows and refs at daemon start, retention by count/age/bytes/refs with `pack-refs --include 'refs/hutan/*'`, never `gc`/`prune`; failure counter and low-disk switch surfaced as state), docs (`docs/architecture.md`, `docs/agents.md` § Checkpoints first lines).

## Acceptance
As spike row CK-2: a fixture chat (`hutan-codex-fixture replay` / ACP fixture) that edits files via a script → one ref per turn, none for a read-only turn; an interrupted/failed turn still gets an end; restart reconciles rows and refs; retention drops oldest first and keeps pre-restore checkpoints; the off switch is effective for the next snapshot; Home / non-git → `unsupported` recorded. Snapshot never blocks a send beyond the 5 s budget (test with a slow fake). `just check` green; commit per step, messages end `(CK-2)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
None.

## Out of scope
Protocol/CLI/MCP (CK-3), restore (CK-4), UI (CK-5); `docs/status.md`, `docs/roadmap.md`.
