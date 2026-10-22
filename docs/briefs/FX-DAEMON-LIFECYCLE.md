# FX-DAEMON-LIFECYCLE — the app manages its daemon: right binary, right version, honest stop, bounded retries

Agent: implementer-opus (Claude Opus 5.5)   Depends on: FX-DAEMON-START merged (start-error record, captured stderr, banner)   Branch: `slice/FX-DAEMON-LIFECYCLE`   Release: 0.2.0

**Contract ownership:** delegated: one additive protocol minor (next free number at merge time) adding daemon identity to `Welcome`, capability `daemon.identity`; golden fixtures appended.

## Context
The user (2026-10-23): "make the app manage the daemon correctly". Investigation `agent://DaemonLifecycleScout` (summary below, file:line there) found:
- The app spawns `hutand` beside its own executable, but **never checks who answers on the socket**: a dev `target/debug/hutand`, an older bundle's daemon, or a formula install is reused silently (`client.rs:1890-1900`).
- **No version check**: `Welcome.daemon_version` is only displayed (`daemon_popover.rs:78`); after `brew upgrade` the old daemon keeps running and the new app talks to it.
- **Autostart retries forever** every ~15–20 s with no backoff cap and no visible error (`supervisor.rs:416-420`, `AutostartTimeout`).
- **"Stopped" is app-local and not persisted** (`workbench/daemon.rs:50`): Stop from the daemon's menu bar item or `hutan daemon stop` is undone within seconds by an open app; with the login item on, the `launchd_grace` fallback spawns `hutand` even after a user stop (`mac.rs:314-319`).
- **Login item** reconciles only on/off, never the registered bundle path (`mac.rs:~226`).
- Stale references: `app.on_quit` (`app.rs:72`), `packaging.md:17` (M6-C wording).

## Scope
1. **Identity on the wire**: `Welcome` gains `daemon_build {version, git_sha, exe_path, started_at_ms}` (additive, capability `daemon.identity`). The desktop knows its own bundled `hutand` (path + the same build id, compiled in via `env!`/build script).
2. **Mismatch handling** on every connect:
   - same build → nothing;
   - different build, **no live sessions** → restart onto the app's own `hutand` automatically (graceful `daemon.shutdown`, wait for exit, spawn), one notice "Updated the background service to 0.2.0";
   - different build **with live sessions** → banner "The background service still runs Hutan 0.1.0 (from <path>). Restart it with 0.2.0? Running terminals are restored; agent turns in progress are interrupted." [Restart now] [Later] (Later: asked again next launch; a newer daemon than the app → "the background service is newer than this app — update the app");
   - a daemon from another install (exe path not this bundle, e.g. a dev build) → banner "Connected to a Hutan service from <path>" [Use this app's service] [Keep]. Never kill a process that is not `hutand`.
   - CLI: `hutan daemon status` prints build + path and the mismatch; `hutan daemon restart` uses the CLI's adjacent `hutand`.
3. **Bounded autostart**: exponential backoff (1 s → 60 s) and, after 3 consecutive failures with the same start-error kind, stop retrying and show FX-DAEMON-START's banner with *Retry*; a successful connect resets it.
4. **Honest stop**: a user stop from any surface (app, daemon menu bar item, `hutan daemon stop`) is announced by the daemon before exit (a `daemon.stopping {by, reason}` event or the existing shutdown notice carrying `by_user`), so every connected app sets *stopped* and does not respawn until the user presses Start (or relaunches the app — relaunch starts it, the app needs it; document this). The `launchd_grace` fallback respects it. The status/menu shows "Stopped by you · Start".
5. **Login item path**: when start-at-login is on and the registered agent's bundle path is not the running bundle (moved/reinstalled), unregister + register again; log it.
6. Cleanup: remove `app.on_quit` leftovers, fix `packaging.md:17`, document the lifecycle in `docs/architecture.md` (who starts what, which binary, mismatch rules, stop semantics) and `docs/packaging.md` (upgrade: the app restarts an idle old daemon itself; with live sessions it asks).

## Acceptance
Tests (real daemon binaries in temp homes): a connect to a daemon reporting another build with no sessions → the app restarts onto its own binary (new epoch, own exe path); with a live session → banner and no restart until *Restart now*; a newer daemon → the update notice; a daemon at a foreign exe path → banner, *Use this app's service* switches; three failing starts → retries stop and the banner shows; `hutan daemon stop` and the menu-bar stop while an app is connected → app shows *Stopped by you* and spawns nothing for ≥ 30 s (test clock); relaunch starts it; login-item path reconcile unit-tested with a fake service. `just check` green; commit per item, messages end `(FX-DAEMON-LIFECYCLE)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
Notarization/signing; remote daemons; `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
