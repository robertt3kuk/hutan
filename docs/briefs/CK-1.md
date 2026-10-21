# CK-1 — checkpoint snapshot engine (daemon module, no protocol, no UI)

Agent: implementer-opus (Claude Opus 5.5) — git plumbing invariants and concurrency   Depends on: ADR 0019 (accepted 2026-10-21)   Branch: `slice/CK-1`

## Context
Read `AGENTS.md`, ADR `docs/adr/0019-agent-checkpoints.md` (Decision 1–12; default on, bounds, privacy rules, never `gc`/`prune`, never cancel a git write), `docs/spikes/2026-10-21-checkpoints.md` §4 (mechanism and measured numbers, invariants §4.3), §7 row **CK-1**, §8 risks (Codex's runaway temp packs #19588/#50920). Existing git runners in `crates/daemon/src/worktrees*` (timeouts, no prompts, no network).

## Scope
`crates/daemon/src/checkpoints/{mod,git,snapshot,caps}.rs` (new module, not wired to sessions yet; a crate-internal API `snapshot(checkout, label) -> Result<Snapshot, CheckpointError>` that later slices call at turn start/end), unit and integration tests under `crates/daemon/tests/it/checkpoints.rs` using temp repos, `docs/architecture.md` (module), `docs/perf.md` (snapshot cost table on this machine).
- Git runner variant: private `GIT_INDEX_FILE` seeded from the user's index (read-only copy), hooks disabled, `GIT_OPTIONAL_LOCKS=0`, no lazy fetch/credential prompts, timeouts; candidates = tracked (modified/deleted) + untracked non-ignored; `lstat` filters before hashing (size cap 10 MiB/file, per-snapshot byte and file caps, `filter=` attributes/LFS, symlinks stored as links, gitlinks and nested repos skipped, secret-name denylist) — every skip is recorded and returned; raw bytes (no eol/ident filters); `write-tree` → `commit-tree` → `update-ref refs/hutan/checkpoints/<session>/<turn>-<start|end>`; per-checkout mutex; a git write is never cancelled in flight.
- Low-disk (< 2 GiB free) and repeated-failure switch-off returned as typed errors.

## Acceptance
- On temp repos: the user's index (bytes and mtime), HEAD, branches, stash, `git status --porcelain` identical before and after a snapshot, also while `.git/index.lock` is held by another process; ignored files never read (assert via a file the test makes unreadable after ignoring it); staged + unstaged + untracked captured; nested repo/gitlink skipped and listed; over-cap files skipped before hashing (no object written); concurrent snapshots on one checkout serialize; a snapshot of an unchanged tree reuses objects.
- Numbers on this machine for a 10k-file temp repo (unchanged, 3 edits, 250 edits, 5 MiB binary) recorded in `docs/perf.md`.
- `just check` green (machine-wide lock); one or two commits ending `(CK-1)`.

## Parity
None (internal module).

## Out of scope
Turn wiring and storage (CK-2), protocol (CK-3), restore (CK-4), UI. Do not edit `core`/`protocol`, `docs/status.md`, `docs/roadmap.md`.

## Report
Summary, API, numbers, invariant tests, files, commands with results, open questions.
