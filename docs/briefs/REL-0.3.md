# REL-0.3 — prepare the 0.3.0 test release (Homebrew)

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/REL-0.3`

**Contract ownership:** none (version bump, release notes, local packaging checks). **Never publish**: no tag, no push, no `gh release`, no tap commit/push — the orchestrator does those.

## Context
User (2026-10-26): "prepare a test release in brew with the current work"; remote hosts stay behind the experimental toggle with a disclaimer (already on master). 0.2.0 was a GitHub pre-release + tap `robertt3kuk/homebrew-hutan` (local clone `~/proj/homebrew-hutan`). Procedure: `docs/packaging.md` § Homebrew (Prepare a release locally, Publish after review) and the tap's `README.md` § Releases and local verification.

## Items (in commit order)
1. **Version**: workspace version `0.2.0` → `0.3.0` (root `Cargo.toml`, `Cargo.lock` via `cargo update -w --offline` or a build), and any test/fixture/doc that pins the current version string (grep `0.2.0`; leave history mentions alone). `CHANGELOG.md`: rename `## Unreleased` to `## 0.3.0 — 2026-10-26` and open an empty `## Unreleased` above it (rule in AGENTS.md: newest on top; released sections never edited). In the 0.3.0 section, make sure the experimental remote-hosts entry keeps its "Experimental — at your own risk" wording. `just check` green; commit `chore(release): 0.3.0`.
2. **Build the release** from that clean commit: `cargo run -p xtask -- release --tap /Users/homie/proj/homebrew-hutan` (it builds, signs ad hoc, verifies, zips, archives HEAD, writes `Casks/hutan.rb` + `Formula/hutan.rb` into the tap clone — leave those tap changes uncommitted). Record both SHA-256s.
3. **Release notes** `target/release-artifacts/NOTES.md`: the 0.3.0 CHANGELOG section, a short "Test release" line at the top (pre-release; may have rough edges), an **Experimental** paragraph repeating the remote-hosts disclaimer and how to turn it on/off, install/upgrade commands (`brew install --cask robertt3kuk/hutan/hutan`, `brew update && brew upgrade --cask hutan`, formula alternative), and the Gatekeeper note exactly as 0.2.0's notes (see `docs/packaging.md` § Security and uninstall; tap README § Gatekeeper).
4. **Local tap verification** exactly as the tap README says: a temporary copy of the tap with `file://` URLs to the generated archives (keep SHA-256s), bundle ids replaced with `dev.hutan.homebrew-test`, committed in the temp copy, tapped as `robertt3kuk/hutan-local`; `brew style`, `brew audit` (record expected online/`--new` failures separately), cask install into a **temporary `--appdir`** (never `/Applications`: the user's own Hutan is there and running), `HUTAN_HOME=<temp> hutan --version` reports 0.3.0, uninstall; formula build-from-source + `hutan --version`/`hutand --version` + `brew test`, uninstall; then untrust/untap and delete the temp copy. Never `brew autoremove`; remove only test-installed build deps.
5. Tap `README.md`: add "Local results for 0.3.0 (2026-10-26)" like the 0.2.0 section (leave uncommitted in `~/proj/homebrew-hutan`).

## Rules
- Work only in `/Users/homie/proj/hutan-wt/REL-0.3` (worktree exists) plus `~/proj/homebrew-hutan` (files only, no commits) and temp dirs. Shell: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- The user's own Hutan runs on this machine (`/Applications/Hutan.app`, `~/Library/Application Support/Hutan`): never touch, quit, signal, or replace it; never `pkill` by name; never install anything into `/Applications`.
- Do not edit `docs/status.md`, `docs/roadmap.md`.
- Report: commit hash of `chore(release): 0.3.0`, both checksums and artifact paths, NOTES.md path, every check with its result, anything that failed.
