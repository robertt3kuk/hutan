# FX-DAEMON-START — a daemon that cannot start says why (and data from a newer Hutan)

Agent: implementer-opus (Claude Opus 5.5)   Depends on: —   Branch: `slice/FX-DAEMON-START`   Release: 0.2.0

**Contract ownership:** none expected (no wire change; the error travels outside the protocol because there is no daemon to speak it).

## Evidence
The user's main machine (the one development started on): the daemon no longer runs. Their `hutand.log` (`~/Library/Application Support/Hutan/…`) ends on 2026-10-20 with "the daemon is stopped from its menu bar item"; nothing is logged after it, although the app was launched again. The database there is at schema **13** (`memory`, migrated 2026-10-19 by a dev build), while the published 0.1.0 (Homebrew) only understands up to **12** — so a 0.1.0 install refuses the database with `SchemaTooNew` (`crates/storage/src/error.rs:19`).
Why it is invisible:
- `crates/daemon/src/main.rs:57-66` prints a startup error only to stderr (`eprintln!`); it never reaches `hutand.log`, although logging is initialised before (`init_logging` at :75).
- Every autostart discards the daemon's stderr (`crates/client/src/client.rs:1925-1928`, `Stdio::null()`), so the desktop/CLI only see "no socket appeared".
- The same applies to any other early failure (lock held, socket path, permissions, a migration error).
[INFERENCE: schema 13 vs 0.1.0 is the most likely cause on that machine; the fix must make whatever the cause is visible.]

## Scope
1. **hutand**: every startup error is also written to `hutand.log` (`error!` with the full chain) before exit, and to a small `hutand-start-error.json` next to the socket (`{at_ms, pid, version, kind, message}`; `kind` ∈ `schema_too_new{found,supported,db}`, `already_running`, `other`), removed on a successful start.
2. **Autostart (client crate)**: stderr goes to a bounded file (`logs/hutand-start.log`, truncated at each autostart, ≤ 64 KiB) instead of `/dev/null`; when the child exits before the socket appears, or the wait times out, the error returned carries the start-error record (or the stderr tail). `hutan daemon start` and every CLI autostart print it; exit code non-zero.
3. **Desktop**: the connection banner/status shows the reason in one sentence with the right action:
   - `schema_too_new`: "Your Hutan data was written by a newer Hutan (database version 13; this build reads up to 12). Update Hutan: `brew upgrade --cask hutan`." plus a secondary *Start with empty data…* (confirm sheet: moves `hutan.db` and its `-wal/-shm` into `backups/hutan-<ts>-v<found>-newer.db`, never deletes; user-only; then restarts the daemon).
   - `already_running` with an unreachable socket: *Show the process* (pid) / *Stop it* (TERM to that exact pid after confirmation).
   - `other`: the message + *Open Logs* + *Copy details*.
   - The daemon menu (*Daemon* menu, ⌘K) shows the same.
4. **CLI**: `hutan daemon status` shows the last start error when the daemon is down; `hutan doctor` (if it exists, else a `hutan daemon why`) prints the record and the log tail. Same *start with empty data* as `hutan daemon reset-data --yes` (moves to backups, prints the path).
5. Docs: `docs/packaging.md` (downgrades: a newer database is refused; how to recover), `docs/architecture.md` (start-error record), troubleshooting section.

## Acceptance
Tests: a daemon started on a database with a newer schema exits non-zero, the log file and the record contain `schema_too_new` with found/supported; CLI autostart prints the sentence and exits non-zero; the desktop (GPUI, real daemon binary on a prepared newer DB) shows the banner and *Start with empty data…* moves the DB into `backups/` and connects; a successful start removes the record. Manual: on a scratch `HUTAN_HOME`, build 0.1.0's `hutand` is not needed — fake the newer schema with `PRAGMA user_version`. `just check` green; commit per item, messages end `(FX-DAEMON-START)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
Downgrade migrations (never supported); `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
