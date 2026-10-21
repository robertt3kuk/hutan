# PR-FINISH — one Finish: commit (+ merge) + push, user-only

Agent: implementer-opus (Claude Opus 5.5)   Depends on: UX-PROTO-1 merged (protocol 0.34)   Branch: `slice/PR-FINISH`

**Contract ownership:** delegated to you: additive protocol minor **0.34 → 0.35**, capability `finish`, golden fixtures appended at the end of the list, router classification `UserOnly`.

## Context
Read `AGENTS.md`, **ADR 0020** (`docs/adr/0020-finish-commit-and-push.md`, the spec), ADR 0016 (`UserOnly`), `docs/orchestration.md` § Review and merge (to amend), `docs/spikes/2026-10-21-product.md` §2 row 6′ and bet card `PR-FINISH`, existing delivery code `crates/desktop/src/workbench/changes.rs`, `changes/delivery.rs`, daemon `crates/daemon/src/worktrees*` (commit/merge, git runner with timeouts, login environment for git credentials).

## Scope
`crates/protocol` (request `worktree.finish {checkout, message, push}` → per-step results `commit`, `merge?`, `push?` each ok/skipped/failed with reason), `crates/daemon` (main checkout: commit all reviewed changes; worktree: commit, merge into target with the existing modes, then push the target; push = `git push` of the current branch to its upstream using the user's own credential helper/SSH agent from the login environment, never force, no rebase; `fetch` read-only only to report behind/ahead before pushing; settings `[finish] push = true`), `crates/client`, `crates/cli` (`hutan finish [--no-push] [-m msg]`), `crates/desktop` (one **Finish** button in Changes for both placements; message prefilled from the session/task title, editable; step-by-step result line with honest failure text: no upstream, rejected non-fast-forward, auth; offer worktree cleanup after), docs (`docs/orchestration.md` amended per ADR 0020, `docs/protocol.md`, `docs/control.md`, `docs/design.md`).

Ride-along in the same 0.35 bump (from E-5's report): `PaneItem` in `crates/protocol` gains an optional `path` for `kind: "file"` tabs, documented in its doc comment and `docs/control.md`; the desktop fills it in `ui.layout`.

## Acceptance
Tests on temp repos with a local bare "origin": main-checkout Finish commits and pushes (remote has the commit); worktree Finish commits, merges and pushes the target; `--no-push` / `push = false` keeps it local; a rejected push leaves the commit and reports `rejected` with the reason; no upstream → reported, commit kept; an agent-originated `worktree.finish` is `forbidden` regardless of `[control]`; nothing is ever force-pushed (assert via the bare repo's reflog).

## Acceptance (common)
Screenshots with Cua Driver per `docs/process.md` § UI checks (before the final gate); `just check` green (machine-wide lock); commit after each logical step; messages end `(PR-FINISH)`. Fresh `HUTAN_HOME` for every run; no `hutand` left.

## Parity
List every moved or removed affordance with its replacement (Definition of Done).

## Out of scope
`docs/status.md`, `docs/roadmap.md`; anything not named here.
