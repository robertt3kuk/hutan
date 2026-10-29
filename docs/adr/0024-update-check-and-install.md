# 0024. Update check and one-click update

- Status: accepted (2026-10-29; the user asked "how can we update it and check for updates — through brew, asking to update?")
- Date: 2026-10-29
- Relates to: ADR 0001 (the daemon owns processes and network work), ADR 0007 (packaging), `docs/packaging.md` § Homebrew, § Updates and the running daemon

## Context
Hutan is distributed as ad-hoc signed GitHub pre-releases and through the upstream tap `robertt3kuk/homebrew-hutan` (cask for the app, formula for CLI + daemon). Today the app never learns that a newer version exists; the only hint is the start-failure text "Update Hutan: `brew upgrade --cask hutan`". After an update the app already says *Updated to x.y.z · What's new* and moves an idle daemon onto the new `hutand` (asking first when sessions run). People install three ways: the cask, the formula, or a bundle copied by hand (as on the maintainer's Mac).

## Decision
1. **The daemon checks**, never the desktop: at most once every 24 h and on *Check for Updates…* (app menu, ⌘K, `hutan update check`), one unauthenticated `GET https://api.github.com/repos/robertt3kuk/hutan/releases` with `User-Agent: Hutan/<version>` — nothing else is sent. Settings: `updates.check = auto | off` (default `auto`, shown in first-run setup and Settings → General with what it contacts), `updates.channel = prerelease | stable` (default `prerelease` while every release is a pre-release). Offline or rate-limited: silent, retried at the next interval.
2. **What is newer**: the highest semver release of the channel above the running version whose assets include `Hutan-<v>-arm64.zip`. The daemon keeps `{version, published, notes_url, notes (the release body, bounded), assets with sha256 digests}` and emits `update.available`; the desktop shows *Update to x.y.z* in the status bar and the app menu, with the release notes in a sheet. Nothing ever updates by itself.
3. **How Hutan was installed decides how it updates** (detected by the daemon from the running bundle path and Homebrew's Caskroom / Cellar, read-only):
   - **Cask**: *Update now* runs `brew update && brew upgrade --cask robertt3kuk/hutan/hutan` in a visible terminal session (the user sees Homebrew's output, its trust prompts and macOS's Gatekeeper dialogs); when it succeeds Hutan offers *Relaunch*.
   - **Formula**: the same with `brew upgrade robertt3kuk/hutan/hutan` (CLI + daemon only).
   - **Bundle copied by hand**: Hutan downloads the release zip, verifies its SHA-256 against **two** sources (the GitHub asset digest and the tap's `Casks/hutan.rb` at the tag), extracts it, checks `codesign --verify --deep --strict`, and after the desktop quits the daemon moves the old bundle to the Trash, puts the new one in place and relaunches it. Any mismatch stops the update and says why; the old app is untouched.
   - Unknown or unwritable location: show the release and the manual command, offer nothing automatic.
4. **Running work**: the existing build-mismatch flow applies after relaunch (idle daemon restarted onto the new `hutand`; with live sessions, *Restart Now / Later*). The update sheet says this before the user confirms.
5. **Protocol**: additive minor, capability `update` (`update.status`, `update.check`, `update.install {method}`, event `update.available`); `update.install` is **UserOnly** (an agent can never update or downgrade Hutan).

## Consequences
- One network endpoint (GitHub API) is contacted on a timer by default; documented in `docs/packaging.md` and switchable off.
- The hand-copied path needs the daemon to outlive the desktop for a moment and to write in the bundle's folder; when it can't (e.g. `/Applications` not writable for a managed Mac), Hutan falls back to the manual command.
- Notarized builds and Sparkle-style signed appcasts would replace parts 2–3 later; revisit when a Developer ID exists.

## Amendment 2026-10-29 (UPDATES-2: how the hand-copied path was built)
- **Two steps, so nothing changes before the user says so**: `update.install {method: bundle}` only downloads, verifies and unpacks into `update-staging/` in the data folder (stage `ready`); the swap is `update.install {…, apply: true, after_pid}`, asked by *Quit and Install*, and runs after that process exited. The brief's "additive fields only" is kept: no new method, a `bundle` request gains `apply`/`after_pid`, and `UpdateInstall` gains `stage`, `downloaded`, `total`, `message`, `manual_command` (protocol stays 0.60, capability `update`). `session_id` and `command` of an install, and `session` of its answer, are optional/empty for a bundle — the only non-additive reading, harmless to an old client because only a new client starts a bundle update.
- **Both digests are required.** GitHub's asset digest and the `sha256` of the tap's `Casks/hutan.rb` must each equal the download's. The tap is read at the release's tag; the tap has not been tagged so far (docs/packaging.md now tags it with each release), so a tap without that tag is read on `main`, where the cask's `version` must be the release's. Beyond the brief: the unpacked bundle must be `dev.hutan.Hutan` at the release's version (a downgrade behind a newer release is refused).
- **A swap that cannot finish does not lose the app.** The new bundle is copied next to the old one, the old one goes to the Trash, the copy is renamed into place; a failing step puts the old one back. The daemon opens the new app (`open -n`) only when it waited for the desktop's process to exit; after a failed swap it opens the old app again so the reason is shown.
- **No ADR-level surprise**: the archive is downloaded by the daemon without the quarantine attribute, so macOS asks nothing about the new app; that is why the two digests and the signature check are the safety, not Gatekeeper.
