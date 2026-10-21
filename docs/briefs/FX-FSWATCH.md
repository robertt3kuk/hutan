# FX-FSWATCH — `fs.changed` loses writes after watching started

Agent: implementer-opus (Claude Opus 5.5)   Depends on: E-10 merged   Branch: `slice/FX-FSWATCH`

## Context
Read `AGENTS.md`, `docs/protocol.md` § fs (E-3), `crates/daemon/src/files/watch.rs` (interest-based `notify` watcher per root, 50 ms coalescing, origin attribution), the E-10 report in `docs/status.md`, and the saved attempt `/tmp/e10-verify-attempt.patch` if it still exists. Symptom (E-10): an e2e test opens a file, a fixture agent turn starts, the test writes the file once with `std::fs::write` (truncate in place) after the turn's approval request — in 10 of 20 runs no `fs.changed` reaches the client; the buffer never reloads. A 1 s verify after registration did not help, so it is not only the startup gap. The committed E-10 test works around it by rewriting until seen — remove that workaround here. The same bug likely affects E-6's reload-on-change. A similar class was fixed in the theme watcher (TH-3 `c84e7b1`: events lost while the FSEvents stream restarts after the watched set changes).

## Scope
`crates/daemon/src/files/*` (and `crates/client` only if delivery is the problem). Instrument with `tracing` (kept at debug level) to see whether the OS event arrives, is filtered, coalesced away, attributed and dropped, or never sent; suspects: the watcher being rebuilt/swapped when a chat starts working in the same folder (worktree/session roots added → new stream), in-place truncate writes vs rename, path canonicalisation (`/private/var` vs `/var`, case), interest keyed by a different path form. Fix the cause; keep a verify pass only if it is part of a correct design (e.g. after any stream restart, compare versions of watched files).

## Acceptance
- The E-10 e2e test writes once and waits on state: 50/50 `--retries 0`.
- New daemon tests: a write right after `fs.watch`; a write while another root/session is added (stream restart); in-place truncate and atomic rename both reported; `/var` vs `/private/var` paths.
- `just check` green; commits end `(FX-FSWATCH)`.

## Parity
None.

## Out of scope
`core`/`protocol` changes (report if needed); `docs/status.md`, `docs/roadmap.md`.
