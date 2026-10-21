# CK-3 — checkpoints on the wire: list, diff, remove

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-2 and UX-QUEUE-STEER (protocol 0.36) merged   Branch: `slice/CK-3`

**Contract ownership:** delegated: protocol 0.36 → **0.37**, capability `checkpoint`, golden fixtures (`just update-golden`, appended), client methods. No restore in this slice.

## Context
Read `AGENTS.md`, ADR 0019, `docs/spikes/2026-10-21-checkpoints.md` §3.3, §3.6, §3.8, §6.1–§6.4 and §7 row **CK-3** (verbatim acceptance), CK-2's code (`crates/daemon/src/checkpoints/`, the SQLite rows, `CheckpointInfo` in `crates/core`), `worktree.diff` (the bounds and binary flag to copy), `crates/cli/src/mcp/table.rs` (MCP tool table), `crates/daemon/src/router.rs` (`Access` per method, `[control]` gate).

## Scope
- `protocol`: `checkpoint.list {session_id?, workspace_id?, limit?≤200, before?} -> {checkpoints, next?}`, `checkpoint.diff {checkpoint_id, against?: "end"|"now"|<checkpoint_id>, path?} -> {files:[{path,status,added,removed,binary,truncated}], patch?}`, `checkpoint.remove {checkpoint_id | session_id} -> {removed}`; events `checkpoint.created`, `checkpoint.removed`, `checkpoint.state`; `ChatInfo.checkpoints? {state: on|off|unsupported, reason?}`. Errors per §6.1. `docs/protocol.md` § Checkpoints.
- `daemon`: handlers over CK-2's store and refs; `against: "now"` diffs the start tree against the checkout through a fresh private-index snapshot (never the user's index); diff bounds like `worktree.diff` (2 MiB per file, 20 MiB total, 5 000 files, binary flag); `remove` deletes refs + rows in one batch and emits `checkpoint.removed`; reads are `Access` read; `remove` from an agent-originated connection goes through `[control]` (asks).
- `client`, `cli`: `hutan checkpoint ls [SESSION|--ws W] [--json]`, `hutan checkpoint diff CHECKPOINT [--against end|now|CP] [--stat] [PATH]`, `hutan checkpoint rm CHECKPOINT|SESSION`.
- MCP (`hutan mcp`): `checkpoint_list`, `checkpoint_diff`, read-only, descriptions say "what the user can rewind to; you cannot restore".
- Docs: `docs/agents.md` § Checkpoints (list/diff/rm, what is captured, privacy), `docs/control.md` (MCP tools, `remove` gate).
- Optional, only if small: E-10's agent-change "before" text may stay as is; do not touch desktop.

## Acceptance
Spike row CK-3: diff bounds and binary flag like `worktree.diff`; an agent-originated `list` works, `remove` asks per `[control]`; CLI against a real daemon with a fixture chat that edits files (`ls` shows one row per changed turn, `diff --stat` matches the edit, `--against now` shows a later user edit, `rm` removes the ref and the row); `checkpoint.created` arrives at a subscribed client at turn end; `ChatInfo.checkpoints` reports `unsupported` with a reason for a non-git folder; an older client without the capability is unaffected (golden fixtures appended). `just check` green; commit per step, messages end `(CK-3)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
CLI ↔ MCP ↔ protocol for list/diff. Desktop comes in CK-5.

## Out of scope
Preview/restore/undo (CK-4), desktop (CK-5), `hutan checkpoint status` (CK-6); `docs/status.md`, `docs/roadmap.md`.
