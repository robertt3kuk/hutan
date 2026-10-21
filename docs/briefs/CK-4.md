# CK-4 — restore files to before a turn: plan, preview, restore, undo

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-3 merged (protocol 0.37)   Branch: `slice/CK-4`

**Contract ownership:** delegated: `restore_plan` and its types in `crates/core`; protocol 0.37 → **0.38** (`checkpoint.preview`, `checkpoint.restore`, `fs.changed` origin `restore`; capability `checkpoint.restore`), golden fixtures appended; client methods.

## Context
Read `AGENTS.md`, ADR 0019, `docs/spikes/2026-10-21-checkpoints.md` §3.6 (shared checkout, busy refusal), §5.1–§5.3 (UX facts the wire must carry, the three-way algorithm, writing rules, the honest label), §6.1–§6.2 (preview/restore shapes, `hutan checkpoint restore|undo`), §6.4 (MCP: `preview` is a read, `restore` not exposed), §7 row **CK-4** (verbatim acceptance), CK-3's code (`checkpoint.*` handlers, private-index snapshot, `crates/daemon/src/checkpoints/`), `fs.changed` origin attribution (E-3, FX-FSWATCH), `context_file.rs` (confined `O_NOFOLLOW` writes), `router.rs` `Access::UserOnly`.

## Scope
- `core`: pure `restore_plan(turns, now) -> Plan` — per path `want`/`last`/`now`, actions `write|delete|skip`, states `clean|after|gap|during|uncaptured`, chains over turns k…n, plan digest. Unit + property tests (no git).
- `daemon`: `checkpoint.preview {checkpoint_id, through?, paths?}` builds the plan from refs + a fresh pre-restore snapshot (never the user's index); `checkpoint.restore {…, accept_conflicts?, plan_digest}` takes the `restore-pre` checkpoint, re-plans and answers `conflict` with the new entries if the digest is stale, writes confined atomic files (mode from the tree entry, raw blobs via `cat-file --batch`, never under `.git/`, symlinks skipped, empty parents removed only when the restore emptied them), takes `restore-post`, records the restore as a synthetic turn so **undo = the same algorithm** (undo-of-undo works); emits `fs.changed` origin `{kind: "restore", checkpoint_id}`; refuses with `invalid_state` while any agent session in the checkout has a running turn; `Access::UserOnly` (agent-originated → `forbidden` regardless of `[control]`); retention keeps pre-restore checkpoints; the user's index and HEAD untouched; `shared_with` and `head_moved` in the preview.
- CLI: `hutan checkpoint restore CHECKPOINT [--through CP] [--path P]... [--accept-conflicts P]... [--yes]` (prints the preview; `[y/N]` default No; needs `--yes` without a terminal; conflicts never overwritten without naming them), `hutan checkpoint undo [SESSION]`. The honest label of §5.3 printed with every preview.
- MCP: `checkpoint_preview` as a read; no restore tool.
- Docs: `docs/protocol.md` § Checkpoints (preview/restore), `docs/agents.md` § Checkpoints (restore, undo, what it will not do), `docs/control.md`.

## Acceptance
Spike row CK-4 on temp repos: a turn edits three files incl. a new one → restore returns the exact start tree (`write-tree` equal); undo restores the end again, and undo-of-undo; a file edited after the turn is listed `after` and untouched without `--accept-conflicts`; `gap` across two turns; stale `plan_digest` → `conflict` with the new plan; agent-originated restore → `forbidden`; a running turn in the checkout → `invalid_state`; a symlink and an oversize (uncaptured) path are skipped with reasons; the user's index bytes, HEAD and stash identical afterwards; an open editor buffer receives `fs.changed` with origin `restore`. CLI against a real daemon: preview, `--yes` restore, undo. `just check` green; commit per step, messages end `(CK-4)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Parity
CLI ↔ protocol for preview/restore/undo; MCP preview only. Desktop is CK-5.

## Out of scope
Desktop (CK-5), `hutan checkpoint status` (CK-6), conversation rewind (CK-7); `docs/status.md`, `docs/roadmap.md`.
