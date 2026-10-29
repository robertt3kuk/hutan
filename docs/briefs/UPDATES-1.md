# UPDATES-1 — update check and brew update

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UPDATES-1`

**Contract ownership:** **delegated**: additive protocol minor (next free at merge — 0.60 today), capability `update`, own commit, goldens.

## Context
ADR 0024 (accepted) — read it fully. User 2026-10-29: "how can we update it and check for updates — through brew, asking to update?"

## Items (in commit order)
1. Protocol `update.status`, `update.check`, `update.install {method}` (UserOnly), event `update.available`.
2. Daemon: the 24 h checker (GitHub releases API, `User-Agent: Hutan/<version>`, bounded body, semver + channel + required asset, asset digests), settings `updates.check` / `updates.channel`, install-method detection (cask / formula / hand-copied bundle / unknown) from the running executable path and Homebrew's Caskroom/Cellar (read-only; never run `brew` to detect), quiet offline/rate-limit handling. The HTTP base URL is configurable for tests (a local mock server serves recorded release JSON); tests never hit GitHub.
3. `update.install` for cask and formula: the daemon starts a visible terminal session running `brew update && brew upgrade --cask robertt3kuk/hutan/hutan` (formula: `brew upgrade robertt3kuk/hutan/hutan`); its exit status is reported; the hand-copied method answers `unsupported` until UPDATES-2.
4. Desktop: status-bar *Update to x.y.z* item, app menu *Check for Updates…*, ⌘K, a sheet with the release notes (rendered Markdown, bounded), what will happen to running sessions, and *Update now* / *Later* / *Skip this version*; after a successful brew run, *Relaunch*. First-run setup row and Settings → General rows for the two settings.
5. CLI `hutan update check|status|install`; docs `docs/packaging.md` (§ Updates: what is contacted, how to turn it off), `docs/design.md`, `docs/protocol.md`.

## Acceptance
Daemon tests with a mock releases server (newer pre-release found, channel stable ignores pre-releases, older/equal ignored, missing asset ignored, offline quiet, 24 h throttle, check on demand), install-method detection from fake layouts, `update.install` UserOnly and the brew session command (fake `brew` on PATH in tests); GPUI tests for the status item, sheet, skip; CLI tests; goldens; Cua screenshots against a mock server (dark + light) — never run a real `brew upgrade` and never touch `/Applications`; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(UPDATES-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/updates1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
